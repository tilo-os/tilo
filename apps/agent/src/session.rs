//! One agent session: join the room, hold the replica, act on instructions.

use std::path::PathBuf;
use ui::time::{Duration, Instant};

use anyhow::{Context, Result};
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
mod native_task;
mod outstanding;

/// Why [`Session::run`] refuses: watching documents for addressed
/// instructions and Chat summons needs each app's document types.
// TODO: not implemented yet: the agent will act through apps' published surfaces via the broker.
pub const DOCUMENT_MODE_UNAVAILABLE: &str = "not implemented yet: the agent will act through \
    apps' published surfaces via the broker (only --task-runs is available)";

/// The default [`Config::pace`], in milliseconds: none. Demos that want the
/// caret to be watched pass `--pace-ms`.
pub const DEFAULT_PACE_MS: u64 = 0;

/// How long a task waits for the broker's pushed action catalogue.
const TASK_CATALOGUE_WAIT: Duration = Duration::from_secs(2);
const TASK_ACTION_WAIT: Duration = Duration::from_secs(30);
/// How long one durable step may stay unsettled before the agent gives up
/// waiting. The host keeps driving the bound child either way.
const TASK_STEP_WAIT: Duration = Duration::from_secs(120);
/// Local wait for one step reply. Longer than the broker's own 10 s step
/// wait so its "retry the same step" answer arrives before this expires.
const TASK_STEP_REPLY_WAIT: Duration = if cfg!(test) {
    Duration::from_millis(300)
} else {
    Duration::from_secs(15)
};
const TASK_STEP_RETRY_PAUSE: Duration = Duration::from_millis(200);
/// The first pause before observing a running invocation; it doubles up to
/// [`FOLLOW_MOST_WAIT`] while the invocation keeps running.
const FOLLOW_FIRST_WAIT: Duration = Duration::from_millis(5);
const FOLLOW_MOST_WAIT: Duration = Duration::from_millis(50);
/// The broker's refusal before any child is bound when its host has no step
/// ledger. Only this refusal may fall back to the legacy invocation path.
const TASK_STEPS_UNAVAILABLE: &str = "enrolled task steps unavailable";

#[derive(Clone, Debug, PartialEq, Eq)]
struct TaskAction {
    app: String,
    peer_id: u64,
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
    /// Rebuild a step's outcome from the host's committed continuation, the
    /// only durable hand-off between enrolled task steps.
    fn from_continuation(continuation: &[u8]) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_slice(continuation)
            .map_err(|_| "the committed step continuation is not valid".to_string())?;
        let output = value
            .get("succeeded")
            .ok_or_else(|| "the committed step did not succeed".to_string())?;
        Ok(Self {
            output: serde_json::from_value(output.clone())
                .map_err(|_| "the committed step output is not valid".to_string())?,
        })
    }

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

/// The host's durable step bound (`MAX_AGENT_TASK_STEPS`).
const MAX_TASK_STEPS: usize = 64;

/// One planned action whose declared incremental Text parameter is written
/// with a live preview, in as few steps as one step's bound allows.
struct ChunkedParagraph<'a> {
    first: &'a TaskAction,
    /// The declared continuation action.
    next: &'a TaskAction,
    /// The incremental Text parameter.
    param: &'a str,
    arguments: &'a serde_json::Map<String, serde_json::Value>,
    text: &'a str,
    /// Most chunks (steps) this paragraph may take.
    budget: usize,
}

/// `arguments` with parameter `param` set to `text`.
fn with_param(
    arguments: &serde_json::Map<String, serde_json::Value>,
    param: &str,
    text: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = arguments.clone();
    arguments.insert(param.into(), serde_json::Value::String(text.into()));
    arguments
}

/// Report one enrolled task update and wait for the broker's answer.
fn report_enrolled(
    link: &mut Link,
    next_request: &mut u64,
    sequence: &mut u64,
    acceptance: runtime_collab::EnrolledTaskAcceptance,
    update: runtime_collab::TaskRunUpdate,
) -> Result<()> {
    *sequence = sequence
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("enrolled sequence exhausted"))?;
    let request_id = *next_request;
    *next_request = request_id
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("request sequence exhausted"))?;
    link.send(&CollabRequest::ReportEnrolledTask {
        request_id,
        acceptance,
        sequence: *sequence,
        update,
    })?;
    link.wait_enrolled_reply(request_id)
}

/// Send one live update as enrolled progress, without waiting for the
/// host's answer; its request id, or `None` when it could not be sent. Best
/// effort: a refusal arrives later and turns live updates off, never the
/// task ([`Session::absorb_refusals`]).
fn send_live(
    link: &mut Link,
    next_request: &mut u64,
    sequence: &mut u64,
    acceptance: &runtime_collab::EnrolledTaskAcceptance,
    message: String,
) -> Option<u64> {
    outstanding::report_enrolled_detached(
        link,
        next_request,
        sequence,
        acceptance.clone(),
        runtime_collab::TaskRunUpdate::Progress { message },
    )
    .inspect_err(|error| tracing::warn!(%error, "live update not sent; live updates stop"))
    .ok()
}

/// What the next task step is built from.
#[derive(Debug, Default)]
enum Handoff {
    #[default]
    None,
    /// A legacy (non-step) invocation's in-memory terminal result.
    Legacy(StepOutcome),
    /// The host's committed continuation for the previous durable step.
    Committed(Vec<u8>),
}

impl Handoff {
    fn outcome(&self) -> Result<Option<StepOutcome>, String> {
        match self {
            Self::None => Ok(None),
            Self::Legacy(outcome) => Ok(Some(outcome.clone())),
            Self::Committed(continuation) => StepOutcome::from_continuation(continuation).map(Some),
        }
    }
}

/// One task-action reply from the collaboration link.
struct TaskReply {
    result: Result<(u64, tilo_actions::InvocationUpdate), String>,
    continuation: Option<Vec<u8>>,
    retry_step: bool,
}

/// Interpret a committed step: success hands its continuation to the next
/// step; a committed failure or cancellation stops the task.
fn committed_step(
    update: &tilo_actions::InvocationUpdate,
    continuation: Vec<u8>,
) -> Result<Handoff, TaskActionStop> {
    let value: serde_json::Value = serde_json::from_slice(&continuation).map_err(|_| {
        TaskActionStop::Failed("the host committed an invalid step continuation".into())
    })?;
    if value.get("succeeded").is_some() {
        StepOutcome::from_continuation(&continuation).map_err(TaskActionStop::Failed)?;
        return Ok(Handoff::Committed(continuation));
    }
    if let Some(code) = value.get("failed") {
        return Err(TaskActionStop::Failed(match update {
            tilo_actions::InvocationUpdate::Failed { error, .. } => error.message.clone(),
            _ => code.as_str().unwrap_or("failed").to_string(),
        }));
    }
    if value.get("cancelled").is_some() {
        return Err(TaskActionStop::Cancelled(match update {
            tilo_actions::InvocationUpdate::Cancelled { message, .. } => message.clone(),
            _ => None,
        }));
    }
    Err(TaskActionStop::Failed(
        "the host committed an unrecognised step continuation".into(),
    ))
}

fn validate_task_run(run: &runtime_collab::TaskRunEnvelope) -> Result<(), String> {
    if run.version != 1 || run.run_id == 0 {
        return Err("The task envelope has an unsupported version or empty run id".into());
    }
    if run.task_id.trim().is_empty()
        || run.task_id.len() > 256
        || run.title.trim().is_empty()
        || run.title.len() > 4 * 1024
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
    let peer_id = offer
        .broker_peer_id
        .ok_or_else(|| "Tasks cannot invoke a local-only action".to_string())?;
    if !matches!(
        offer.descriptor.contract.confirmation,
        tilo_actions::ConfirmationPolicy::None
    ) {
        return Err("Tasks cannot invoke an action requiring human confirmation".into());
    }
    if !offer.wants_content() {
        tilo_actions::ActionArgs::bind_json(&offer.descriptor.params, &arguments)
            .map_err(|error| format!("The planned action arguments are invalid: {error}"))?;
    }
    Ok(TaskAction {
        app: offer.app.clone(),
        peer_id,
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
    /// Pause after each visible action and edit, so people can watch the
    /// caret work. A demo affordance: nothing waits on it, so the default
    /// ([`DEFAULT_PACE_MS`]) is none.
    pub pace: Duration,
    /// Stop after the first instruction has been carried out.
    pub once: bool,
    /// Let a model choose which exported action answers an instruction.
    /// False pins the run to the keyword vocabulary.
    pub model: bool,
    /// How long that call may take before the keyword vocabulary decides
    /// instead. Short on purpose: the work must not wait on the network.
    pub model_timeout: Duration,
    /// How long to wait for somebody in the room to answer with the
    /// document before giving up and saying which address went
    /// unanswered. `Duration::ZERO` waits forever — right for an agent
    /// left running against a room whose document nobody has opened yet,
    /// wrong for a scripted run, where it turns a mismatched route into
    /// a hang that names no cause.
    pub await_document: Duration,
    /// How long a paragraph's text must stop changing before it is read as
    /// a finished request. `Duration::ZERO` acts on the first sight of it —
    /// right for a page seeded before the agent joins, wrong for one a
    /// person is actively typing into, where every keystroke would else
    /// race the agent's own read.
    pub settle: Duration,
    /// What the agent may enumerate and perform. A denied operation is
    /// absent from what it discovers, never merely refused — see
    /// [`crate::surface::offers`].
    pub permissions: PermissionContext,
    /// The agent's content store, created once when it starts: content an
    /// operation produced is read from here by the operation consuming it.
    pub content: tilo_content::ContentStore,
}

pub struct Session {
    cfg: Config,
    link: Link,
    /// Decides which exported action answers an instruction.
    chooser: Chooser,
    enrolled_custody: Option<runtime_collab::EnrolledRecipientCustody>,
    enrolled_active: Option<runtime_collab::EnrolledTaskAcceptance>,
    enrolled_sequence: u64,
    /// Monotonic deadline from an explicit broker-minted owner task policy.
    task_deadline: Option<Instant>,
    native_task: Option<native_task::NativeTaskState>,
    /// The broker's pushed, permission-filtered live action directory the
    /// task loop surveys.
    broker_surfaces: Vec<ui::collaboration::BrokerActionSurface>,
    next_broker_request: u64,
    /// Live activity for the enrolled task in progress (display only).
    live: crate::live::LivePublisher,
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
            // A managed (enrolled) agent is limited to its task-bound broker surface: the broker
            // refuses the cluster route, the late-join snapshot and presence on that descriptor, and
            // one refused snapshot RPC shuts down the whole collaboration bridge. Its catalogue
            // arrives through the enrolled ActionCatalogue instead.
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
        let (chooser, enrolled_custody) = match managed {
            Some((selection, custody)) => {
                let chooser = Chooser::with_provider(true, cfg.model_timeout, selection);
                anyhow::ensure!(chooser.is_live(), "managed model gateway unavailable");
                (chooser, Some(custody))
            }
            None => (
                default_selection().map_or_else(
                    || Chooser::new(false, cfg.model_timeout),
                    |selection| Chooser::with_provider(cfg.model, cfg.model_timeout, selection),
                ),
                None,
            ),
        };
        Ok(Self {
            cfg,
            link,
            chooser,
            enrolled_custody,
            enrolled_active: None,
            enrolled_sequence: 0,
            task_deadline: None,
            native_task: None,
            broker_surfaces: Vec::new(),
            next_broker_request: 1,
            live: crate::live::LivePublisher::default(),
            memory: agent_loop::Memory::default(),
            broker_revision: 0,
            catalogue_cache: None,
            turn: None,
            outstanding: outstanding::Outstanding::default(),
        })
    }

    /// Handle addressed document instructions and Chat summons in the room.
    ///
    /// # Errors
    ///
    /// Always, for now: see [`DOCUMENT_MODE_UNAVAILABLE`].
    pub fn run(&mut self) -> Result<()> {
        anyhow::bail!(DOCUMENT_MODE_UNAVAILABLE)
    }

    /// Run only broker-addressed Task Center leases. This is deliberately a
    /// separate mode from document listening: a title is not a document
    /// instruction, and an unattended task may invoke only actions that the
    /// authenticated broker currently publishes to this agent.
    ///
    /// # Errors
    ///
    /// Returns an error when receiving catalogue updates or reporting task
    /// progress through the collaboration link fails.
    pub fn run_task_runs(&mut self) -> Result<()> {
        let mut transforms = crate::transform::TransformService::new(
            std::sync::Arc::new(crate::transform::GatewayModel),
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
        if self.enrolled_custody.is_some() {
            let request_id = self.next_enrolled_request()?;
            self.link
                .send(&CollabRequest::SubscribeEnrolledTasks { request_id })?;
            self.link.wait_enrolled_reply(request_id)?;
        }
        tracing::info!(agent = %self.cfg.name, "waiting for broker-addressed task runs");
        loop {
            match self.link.recv(Duration::from_millis(100))? {
                Some(CollabEvent::ActionCatalogue { surfaces }) => {
                    self.set_broker_surfaces(surfaces);
                }
                Some(CollabEvent::EnrolledTask(run)) => self.run_enrolled_task(&run)?,
                Some(CollabEvent::EnrolledTaskDisconnected { detail }) => anyhow::bail!(detail),
                Some(CollabEvent::TaskRun(run))
                    if self.enrolled_active.is_none()
                        && (self.enrolled_custody.is_none()
                            || runtime_collab::native_task_revision(&run.lease).is_some()) =>
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
                    resource,
                    action,
                    argument,
                    actor,
                    context,
                    ..
                }) => {
                    let update = transforms.start(
                        id,
                        &resource,
                        &action,
                        argument.as_deref(),
                        &actor,
                        context.as_deref(),
                    );
                    self.link
                        .send(&CollabRequest::UpdateInvocation { id, update })?;
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
        reason = "one task run keeps planning, per-step reporting, chunked writing and the terminal update in a single auditable sequence"
    )]
    fn run_task(&mut self, run: &runtime_collab::TaskRunEnvelope) -> Result<()> {
        if self.enrolled_active.is_none() {
            self.task_deadline = run.execution_budget.and_then(|budget| {
                ui::time::now().checked_add(Duration::from_millis(u64::from(budget.total_ms)))
            });
        }
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
        let validation = if self.enrolled_active.is_some() {
            validate_enrolled_context(run)
        } else {
            validate_task_run(run)
        };
        if let Err(message) = validation {
            tracing::warn!(run_id = run.run_id, %message, "refused malformed task run");
            self.report_task(run, runtime_collab::TaskRunUpdate::Failed { message })?;
            return Ok(());
        }
        self.begin_task_watch();
        if self.enrolled_active.is_none() {
            self.report_task(run, runtime_collab::TaskRunUpdate::Accepted)?;
        } else {
            // Live activity rides the enrolled progress carrier only.
            self.live.start();
            self.live
                .set_state(crate::live::LiveState::Thinking, "Thinking", None);
            self.live_publish(true);
        }
        // None of these three waits for the host: the model call is the long
        // pole, and the planner below starts it at once. The cancellation
        // answer is read before the first step (or the first planned action),
        // which it reaches the host ahead of.
        self.report_task(
            run,
            runtime_collab::TaskRunUpdate::Progress {
                message: "Reviewing the permission-filtered action catalogue".into(),
            },
        )?;
        self.poll_cancellation(run)?;
        self.wait_for_task_catalogue()?;
        // A model task is a discover → invoke → observe loop over the live
        // catalogue. A managed agent has no other planner; a legacy one
        // without a model falls back to the rules-only survey below.
        if matches!(run.planning, runtime_collab::TaskRunPlanning::Model)
            && (self.chooser.loop_model().is_ok() || self.enrolled_custody.is_some())
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
                    if let Some(reply) = &finish.reply {
                        self.live.set_reply(reply, true);
                    }
                    let message = match finish.reply {
                        // The model's own reply is the durable chat answer. The
                        // legacy Task Center carrier bounds it to 280 characters.
                        Some(reply) if self.enrolled_active.is_some() => reply,
                        Some(reply) => reply.chars().take(280).collect(),
                        None => format!("Completed {completed} broker-approved action(s)"),
                    };
                    self.live_end(crate::live::LiveState::Done, "Done");
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
        // Enrolled tasks run as durable host steps: each next step is built
        // from the host's committed continuation, never an in-memory reply.
        let mut stepped = self.enrolled_active.is_some();
        let mut handoff = Handoff::None;
        if let Some(reply) = &reply {
            self.live.set_reply(reply, true);
        }
        // Durable step indices: one per invoked step, so a chunked paragraph
        // takes several. Without chunking this equals the action index.
        let mut step_index = 0_usize;
        for (index, action) in actions.iter().enumerate() {
            if self.task_cancelled(run)? {
                return self.cancel_task(run, "Cancelled before the next action");
            }
            let arguments = match handoff.outcome().and_then(|previous| {
                arguments_for_step(
                    action.carries.as_deref(),
                    &action.arguments,
                    previous.as_ref(),
                )
            }) {
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
                },
            )?;
            let result = if let Some((param, text, next)) = self.chunkable(action, &arguments) {
                // Steps still needed after this action, one each.
                let later = actions.len() - index - 1;
                let budget = MAX_TASK_STEPS
                    .saturating_sub(step_index + later)
                    .clamp(1, crate::live::MAX_CHUNKS);
                let paragraph = ChunkedParagraph {
                    first: action,
                    next: &next,
                    param: &param,
                    arguments: &arguments,
                    text: &text,
                    budget,
                };
                self.run_chunked_paragraph(run, index, &mut step_index, &paragraph, &mut stepped)?
            } else {
                self.live_applying(action, &arguments, None);
                let result =
                    self.run_task_step(run, index, step_index, action, &arguments, &mut stepped)?;
                step_index += 1;
                result
            };
            match result {
                Ok(next) => {
                    self.chooser
                        .mark(tilo_model_gateway::TraceEvent::ActionSettled, index);
                    handoff = next;
                }
                Err(TaskActionStop::Cancelled(message)) => {
                    let detail = if self.enrolled_active.is_some() {
                        message
                            .as_deref()
                            .unwrap_or("Cancelled while an action was running")
                    } else {
                        "Cancelled while an action was running"
                    };
                    return self.cancel_task(run, detail);
                }
                Err(TaskActionStop::Failed(message)) => return self.fail_task(run, &message),
            }
        }
        let message = match reply {
            // The model's own reply is the durable chat answer. The legacy
            // Task Center carrier bounds messages to 280 characters.
            Some(reply) if self.enrolled_active.is_some() => reply,
            Some(reply) => reply.chars().take(280).collect(),
            None => format!("Completed {} broker-approved action(s)", actions.len()),
        };
        self.live_end(crate::live::LiveState::Done, "Done");
        self.report_task(run, runtime_collab::TaskRunUpdate::Succeeded { message })
    }

    /// The live broker offers of one publishing peer — all an action of that
    /// peer can be looked up in, without building every peer's offers.
    fn peer_offers(&self, peer: u64) -> impl Iterator<Item = surface::Offer> + '_ {
        self.broker_surfaces
            .iter()
            .filter(move |surface| surface.peer_id == peer)
            .flat_map(|surface| surface::broker_offers(std::slice::from_ref(surface)))
    }

    /// The broker offer `action` was built from.
    fn offer_of(&self, action: &TaskAction) -> Option<surface::Offer> {
        self.peer_offers(action.peer_id).find(|offer| {
            offer.broker_peer_id == Some(action.peer_id)
                && offer.descriptor.address.resource == action.resource
                && offer.descriptor.address.action == action.action
        })
    }

    /// Whether `action` is written as a paragraph with a live preview (in
    /// parts only past one step's bound): it writes text in parts
    /// ([`crate::live::continuation`]) longer than
    /// [`crate::live::CHUNK_MIN_TEXT`], and its continuation is published on
    /// the same resource with that Text parameter. Only enrolled tasks chunk. Returns the parameter,
    /// its text, and the continuation step.
    fn chunkable(
        &self,
        action: &TaskAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<(String, String, TaskAction)> {
        if self.enrolled_active.is_none() || action.carries.is_some() {
            return None;
        }
        let offer = self.offer_of(action)?;
        let (param, next) = self.continuation_of(&offer)?;
        let text = arguments.get(&param)?.as_str()?;
        if text.len() <= crate::live::CHUNK_MIN_TEXT {
            return None;
        }
        Some((param, text.to_owned(), next))
    }

    /// The Text parameter `offer` writes in parts
    /// ([`crate::live::continuation`]) and its continuation step, when
    /// that continuation is published on the same resource with the same
    /// Text parameter.
    fn continuation_of(&self, offer: &surface::Offer) -> Option<(String, TaskAction)> {
        let peer = offer.broker_peer_id?;
        let (param, next) = crate::live::continuation(&offer.descriptor)?;
        let text_param = |offer: &surface::Offer| {
            offer.descriptor.params.iter().any(|candidate| {
                candidate.name == param && candidate.ty == tilo_actions::ParamType::Text
            })
        };
        if !text_param(offer) {
            return None;
        }
        let continuation = self.peer_offers(peer).find(|candidate| {
            candidate.broker_peer_id == Some(peer)
                && candidate.descriptor.address.resource == offer.descriptor.address.resource
                && candidate.descriptor.address.action == next
                && text_param(candidate)
        })?;
        let mut sample = serde_json::Map::new();
        sample.insert(param.clone(), serde_json::Value::String("…".into()));
        let next = task_action_from_offer(&continuation, sample).ok()?;
        Some((param, next))
    }

    /// Write one incremental text that is already complete. It is one step
    /// through the planned action — each step is a durable round trip, so
    /// splitting complete text only makes it land later — unless one step
    /// cannot carry it ([`crate::live::step_text_len`]); then the rest goes
    /// through the declared continuation, in order, through the unchanged
    /// durable step path. Before each step the still-uncommitted rest is
    /// sent as the live preview.
    fn run_chunked_paragraph(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        index: usize,
        step_index: &mut usize,
        paragraph: &ChunkedParagraph<'_>,
        stepped: &mut bool,
    ) -> Result<Result<Handoff, TaskActionStop>> {
        let mut rest = paragraph.text;
        let mut chunks_left = paragraph.budget;
        let mut last = Handoff::None;
        let mut first = true;
        while !rest.is_empty() {
            if !first && self.task_cancelled(run)? {
                return Ok(Err(TaskActionStop::Cancelled(Some(
                    "Cancelled before the next action".into(),
                ))));
            }
            let len = crate::live::step_text_len(rest, chunks_left);
            let (chunk, after) = rest.split_at(len);
            let (action, arguments) = if first {
                (
                    paragraph.first,
                    with_param(paragraph.arguments, paragraph.param, chunk),
                )
            } else {
                (
                    paragraph.next,
                    with_param(&paragraph.next.arguments, paragraph.param, chunk),
                )
            };
            self.live_applying(paragraph.first, paragraph.arguments, Some(rest));
            let result =
                self.run_task_step(run, index, *step_index, action, &arguments, stepped)?;
            *step_index += 1;
            match result {
                Ok(next) => last = next,
                Err(stop) => return Ok(Err(stop)),
            }
            chunks_left = chunks_left.saturating_sub(1);
            rest = after;
            first = false;
        }
        self.live.set_preview(None);
        Ok(Ok(last))
    }

    /// Invoke one step: durable when the host keeps a step ledger, else
    /// through the legacy invocation path.
    fn run_task_step(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        index: usize,
        step_index: usize,
        action: &TaskAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
        stepped: &mut bool,
    ) -> Result<Result<Handoff, TaskActionStop>> {
        let step = if *stepped {
            Some(u32::try_from(step_index).context("task plan exceeds the step range")?)
        } else {
            None
        };
        // A cancellation poll already out answers ahead of this step in the
        // client's one ordered queue: honouring it costs no round trip.
        if self.cancelled_before_step()? {
            return Ok(Err(TaskActionStop::Cancelled(Some(
                "Cancelled before the next step".into(),
            ))));
        }
        self.chooser
            .mark(tilo_model_gateway::TraceEvent::ActionSubmitted, index);
        let settled = match step {
            Some(step) => self.invoke_task_step(run, step, action, arguments, stepped)?,
            None => self
                .invoke_task_action(run, action, arguments)?
                .map(Handoff::Legacy),
        };
        self.step_settled();
        Ok(settled)
    }

    /// Show `action` being applied, labelled from its own descriptor.
    /// `pending` is its text not yet committed; without it, the value of its
    /// first Text-typed parameter is shown.
    fn live_applying(
        &mut self,
        action: &TaskAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
        pending: Option<&str>,
    ) {
        if !self.live.is_active() {
            return;
        }
        let offer = self.offer_of(action);
        let pending = pending.map(str::to_owned).or_else(|| {
            offer.as_ref()?.descriptor.params.iter().find_map(|param| {
                (param.ty == tilo_actions::ParamType::Text)
                    .then(|| arguments.get(param.name.as_ref())?.as_str())
                    .flatten()
                    .map(str::to_owned)
            })
        });
        let record = self
            .broker_surfaces
            .iter()
            .find(|surface| surface.peer_id == action.peer_id)
            .map(|surface| surface.record_id.clone())
            .unwrap_or_default();
        self.live.set_state(
            crate::live::LiveState::Applying,
            &crate::live::action_label(&action.label, &action.resource_label),
            None,
        );
        self.live
            .set_preview(pending.map(|text| crate::live::LivePreview {
                app: action.app.clone(),
                record,
                text,
            }));
        self.live_publish(true);
    }

    /// Send the live view: `flush` sends any change now, otherwise at most
    /// [`crate::live::MAX_UPDATES_PER_SECOND`] per second. Best effort, and
    /// never waits for the host: while one periodic update is unanswered the
    /// next coalesces in the publisher rather than queueing behind it.
    fn live_publish(&mut self, flush: bool) {
        self.absorb_refusals();
        let Some(acceptance) = self.enrolled_active.clone() else {
            return;
        };
        if !self.live.is_active() || (!flush && self.live_in_flight()) {
            return;
        }
        let Session {
            live,
            link,
            next_broker_request,
            enrolled_sequence,
            outstanding,
            ..
        } = self;
        let mut send = |message: String| {
            send_live(
                link,
                next_broker_request,
                enrolled_sequence,
                &acceptance,
                message,
            )
            .map(|request_id| outstanding.record_live(request_id))
            .is_some()
        };
        if flush {
            live.flush(ui::time::now(), &mut send);
        } else {
            live.tick(ui::time::now(), &mut send);
        }
    }

    /// The last live update of a task, sent before its terminal report
    /// (the carrier refuses progress after it).
    fn live_end(&mut self, state: crate::live::LiveState, label: &str) {
        if self.live.is_active() {
            self.live.set_state(state, label, None);
            self.live_publish(true);
        }
        self.live.stop();
    }

    fn report_task(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        update: runtime_collab::TaskRunUpdate,
    ) -> Result<()> {
        if let Some(acceptance) = self.enrolled_active.clone() {
            if matches!(update, runtime_collab::TaskRunUpdate::Progress { .. }) {
                // Progress is display: it reaches the host in order without
                // the task waiting for each answer. A refusal arrives later
                // and turns live updates off. A terminal report still waits:
                // it ends the task's custody.
                outstanding::report_enrolled_detached(
                    &mut self.link,
                    &mut self.next_broker_request,
                    &mut self.enrolled_sequence,
                    acceptance,
                    update,
                )?;
                return Ok(());
            }
            return report_enrolled(
                &mut self.link,
                &mut self.next_broker_request,
                &mut self.enrolled_sequence,
                acceptance,
                update,
            );
        }
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
        self.live_end(crate::live::LiveState::Failed, "Needs attention");
        self.report_task(
            run,
            runtime_collab::TaskRunUpdate::Failed {
                message: message.to_string(),
            },
        )
    }

    fn cancel_task(&mut self, run: &runtime_collab::TaskRunEnvelope, message: &str) -> Result<()> {
        self.live_end(crate::live::LiveState::Failed, "Cancelled");
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
        let (offer, arguments) = surface::from_verbs(&run.title, &catalogue.offers)
            .map_err(|error| format!("Rules-only planner could not choose an action: {error:?}"))?;
        task_action_from_offer(offer, arguments).map(|action| (vec![action], None))
    }

    /// Run one durable step of an enrolled task. Every attempt re-sends the
    /// identical request (same request id, step and arguments), so the host
    /// returns the one child it bound for this step instead of starting a new
    /// invocation. Without a host step ledger this falls back, once and for
    /// the rest of the run, to the legacy invocation path.
    fn invoke_task_step(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        step: u32,
        action: &TaskAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
        stepped: &mut bool,
    ) -> Result<Result<Handoff, TaskActionStop>> {
        if self.task_budget_expired() {
            return Ok(Err(TaskActionStop::Failed(
                "Task time budget expired before starting a step".into(),
            )));
        }
        let request_id = self.next_request_id();
        // One invocation id for every attempt: a retry is the same request.
        let invocation = tilo_actions::InvocationId::new();
        let request = CollabRequest::InvokeTaskAction {
            request_id,
            invocation,
            run_id: run.run_id,
            lease: self.task_lease(run),
            step: Some(step),
            peer_id: action.peer_id,
            resource: action.resource.clone(),
            action: action.action.clone(),
            arguments: arguments.clone(),
        };
        let deadline = self.task_wait_deadline(TASK_STEP_WAIT);
        loop {
            if self.task_budget_expired() {
                self.link.send(&CollabRequest::RemoteAction {
                    message: RemoteActionMessage::Cancel { invocation },
                })?;
                return Ok(Err(TaskActionStop::Failed(
                    "Task time budget expired; cancellation requested, step outcome remains unknown".into())));
            }
            self.link.send(&request)?;
            match self.wait_for_task_action_reply(request_id, TASK_STEP_REPLY_WAIT)? {
                // No reply or a pending step: the outcome is uncertain, so
                // only the same step may be asked again.
                None
                | Some(TaskReply {
                    retry_step: true, ..
                }) => {}
                Some(TaskReply {
                    result: Ok((_, update)),
                    continuation: Some(continuation),
                    ..
                }) => {
                    self.publish_task_interaction(run, action, &update, 0)?;
                    return Ok(committed_step(&update, continuation));
                }
                Some(TaskReply {
                    result: Ok(_),
                    continuation: None,
                    ..
                }) => {
                    return Ok(Err(TaskActionStop::Failed(
                        "Task step reply carried no committed continuation".into(),
                    )));
                }
                Some(TaskReply {
                    result: Err(error), ..
                }) if error.contains(TASK_STEPS_UNAVAILABLE) => {
                    // Refused before any child was bound; nothing ran.
                    *stepped = false;
                    return Ok(self
                        .invoke_task_action(run, action, arguments)?
                        .map(Handoff::Legacy));
                }
                Some(TaskReply {
                    result: Err(error), ..
                }) => {
                    return Ok(Err(TaskActionStop::Failed(error)));
                }
            }
            if ui::time::now() >= deadline {
                return Ok(Err(TaskActionStop::Failed(
                    "Task step did not settle before its deadline; its outcome is still unknown"
                        .into(),
                )));
            }
            std::thread::sleep(TASK_STEP_RETRY_PAUSE);
        }
    }

    fn invoke_task_action(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        action: &TaskAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Result<StepOutcome, TaskActionStop>> {
        if self.task_budget_expired() {
            return Ok(Err(TaskActionStop::Failed(
                "Task time budget expired before invoking an action".into(),
            )));
        }
        let request_id = self.next_request_id();
        let invocation = self.task_child_id(run)?;
        self.link.send(&CollabRequest::InvokeTaskAction {
            request_id,
            invocation,
            run_id: run.run_id,
            lease: self.task_lease(run),
            step: None,
            peer_id: action.peer_id,
            resource: action.resource.clone(),
            action: action.action.clone(),
            arguments: arguments.clone(),
        })?;
        let update = match self.wait_for_task_action_result(request_id)? {
            Ok((_, update)) => update,
            Err(error) => return Ok(Err(TaskActionStop::Failed(error))),
        };
        if self.native_task.is_some() {
            self.follow_native_invocation(run, action, arguments, invocation, update)
        } else {
            self.follow_task_invocation(run, action, invocation, update)
        }
    }

    /// Follow a started legacy invocation to its terminal update through
    /// the action host.
    fn follow_task_invocation(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        action: &TaskAction,
        invocation: tilo_actions::InvocationId,
        mut update: tilo_actions::InvocationUpdate,
    ) -> Result<Result<StepOutcome, TaskActionStop>> {
        let mut interaction_sequence = 0_u64;
        self.publish_task_interaction(run, action, &update, interaction_sequence)?;
        let deadline = self.task_wait_deadline(TASK_ACTION_WAIT);
        let mut wait = FOLLOW_FIRST_WAIT;
        while !update.is_terminal() && ui::time::now() < deadline {
            let left = deadline.saturating_duration_since(ui::time::now());
            let pushed = self.await_invocation_news(invocation, wait.min(left))?;
            wait = (wait * 2).min(FOLLOW_MOST_WAIT);
            // Asked without waiting: the answer arrives ahead of the
            // observation's in the client's one ordered queue, so it is read
            // below as fresh as the observation itself.
            self.poll_cancellation(run)?;
            update = match pushed {
                Some(pushed) => pushed,
                None => match self.observe(invocation)? {
                    Ok(update) => update,
                    Err(error) => return Ok(Err(TaskActionStop::Failed(error))),
                },
            };
            interaction_sequence = interaction_sequence.saturating_add(1);
            self.publish_task_interaction(run, action, &update, interaction_sequence)?;
            if !update.is_terminal() && self.cancellation_known()? {
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
                interaction_sequence = interaction_sequence.saturating_add(1);
                let cancelled = tilo_actions::InvocationUpdate::Cancelled {
                    invocation: update.invocation(),
                    message: Some("Cancelled from Task Center".into()),
                };
                self.publish_task_interaction(run, action, &cancelled, interaction_sequence)?;
                return Ok(Err(TaskActionStop::Cancelled(Some(
                    "Cancelled from Task Center".into(),
                ))));
            }
        }
        if self.task_budget_expired() && !update.is_terminal() {
            self.link.send(&CollabRequest::RemoteAction {
                message: RemoteActionMessage::Cancel { invocation },
            })?;
            return Ok(Err(TaskActionStop::Failed(
                "Task time budget expired; cancellation requested, action outcome remains unknown"
                    .into(),
            )));
        }
        match update {
            tilo_actions::InvocationUpdate::Succeeded { output, .. } => {
                Ok(Ok(StepOutcome { output }))
            }
            tilo_actions::InvocationUpdate::Cancelled { message, .. } => {
                Ok(Err(TaskActionStop::Cancelled(message)))
            }
            tilo_actions::InvocationUpdate::Failed { error, .. } => {
                Ok(Err(TaskActionStop::Failed(error.message)))
            }
            other => {
                let message = "Task action did not reach a terminal state before its deadline";
                interaction_sequence = interaction_sequence.saturating_add(1);
                self.publish_task_interaction(
                    run,
                    action,
                    &tilo_actions::InvocationUpdate::Failed {
                        invocation: other.invocation(),
                        error: tilo_actions::ActionFailure {
                            code: "timeout".into(),
                            message: message.into(),
                            details: None,
                            retryable: true,
                        },
                    },
                    interaction_sequence,
                )?;
                Ok(Err(TaskActionStop::Failed(message.into())))
            }
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
    ) -> Result<()> {
        if self.enrolled_custody.is_some() || self.enrolled_active.is_some() {
            // No managed task diagnostic is constructed as public presence.
            // Terminal detail returns to run_task's single private terminal report.
            if let tilo_actions::InvocationUpdate::Progress { phase, message, .. } = update {
                anyhow::ensure!(
                    self.enrolled_active.is_some(),
                    "managed action lacks accepted task custody"
                );
                for detail in [Some(phase), message.as_ref()].into_iter().flatten() {
                    if !detail.is_empty() {
                        self.report_task(
                            run,
                            runtime_collab::TaskRunUpdate::Progress {
                                message: detail.clone(),
                            },
                        )?;
                    }
                }
            }
            return Ok(());
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
        Ok(())
    }

    fn wait_for_task_action_result(
        &mut self,
        request_id: u64,
    ) -> Result<Result<(u64, tilo_actions::InvocationUpdate), String>> {
        Ok(
            match self.wait_for_task_action_reply(request_id, Duration::from_secs(10))? {
                Some(reply) => reply.result,
                None => Err("the broker did not answer the task action request".into()),
            },
        )
    }

    /// Wait up to `wait` for the reply to `request_id`; `None` on timeout.
    fn wait_for_task_action_reply(
        &mut self,
        request_id: u64,
        wait: Duration,
    ) -> Result<Option<TaskReply>> {
        let deadline = self.task_wait_deadline(wait);
        let mut deferred = Vec::new();
        loop {
            // Keep a streaming model turn moving while the step settles.
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
                Some(CollabEvent::TaskActionInvocationResult {
                    request_id: received,
                    result,
                    continuation,
                    retry_step,
                }) if received == request_id => {
                    self.link.defer(deferred);
                    return Ok(Some(TaskReply {
                        result,
                        continuation,
                        retry_step,
                    }));
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

    /// Wait up to `wait` for news of `invocation`: its update, when the host
    /// pushes one (returned), or a document change in the room (absorbed;
    /// the wait ends so the caller asks at once). Catalogue updates are
    /// applied and every other event keeps its place.
    fn await_invocation_news(
        &mut self,
        invocation: tilo_actions::InvocationId,
        wait: Duration,
    ) -> Result<Option<tilo_actions::InvocationUpdate>> {
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
                }) if answered == invocation => {
                    pushed = Some(state.update);
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

fn managed_context(
    cfg: &Config,
) -> Result<
    Option<(
        crate::providers::ProviderSelection,
        runtime_collab::EnrolledRecipientCustody,
    )>,
> {
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
    let custody = runtime_collab::EnrolledRecipientCustody::new(
        text("agent")?,
        text("launch")?,
        text("runtime_epoch")?,
        text("channel_id")?,
    )
    .map_err(anyhow::Error::msg)?;
    Ok(Some((selection, custody)))
}
fn validate_enrolled_context(run: &runtime_collab::TaskRunEnvelope) -> Result<(), String> {
    let context = serde_json::from_value::<tilo_actions::ActionContext>(run.action_context.clone())
        .map_err(|_| "invalid enrolled context")?;
    if context.purpose.is_some()
        || !context.group.as_ref().is_some_and(|group| {
            group.id == run.task_id && group.label == run.task_id && group.kind == "task"
        })
    {
        return Err("enrolled context must carry opaque task identity only".into());
    }
    Ok(())
}
impl Session {
    fn next_enrolled_request(&mut self) -> Result<u64> {
        let id = self.next_broker_request;
        self.next_broker_request = id
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("request sequence exhausted"))?;
        Ok(id)
    }
    fn run_enrolled_task(&mut self, run: &runtime_collab::EnrolledTaskEnvelope) -> Result<()> {
        validate_enrolled_context(&run.task).map_err(anyhow::Error::msg)?;
        let custody = self
            .enrolled_custody
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("enrolled task sent to legacy agent"))?;
        let decision = custody.accept(run).map_err(anyhow::Error::msg)?;
        let request_id = self.next_enrolled_request()?;
        self.link.send(&CollabRequest::AcceptEnrolledTask {
            request_id,
            acceptance: decision.acceptance.clone(),
        })?;
        self.link.wait_enrolled_reply(request_id)?;
        if !decision.execute {
            return Ok(());
        }
        self.enrolled_active = Some(decision.acceptance);
        self.enrolled_sequence = 0;
        // The host's durable record of this agent's earlier tasks replaces
        // what this process remembers: it survives restarts and holds the
        // exact operations each step ran.
        if let Some(history) = &run.history
            && !self.memory.restore(history)
        {
            tracing::warn!("ignored an unreadable agent history");
        }
        let result = self.run_task(&run.task);
        self.enrolled_active = None;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_run() -> runtime_collab::TaskRunEnvelope {
        runtime_collab::TaskRunEnvelope {
            version: 1,
            task_id: "task:review".into(),
            title: "Review draft".into(),
            run_id: 7,
            lease: "broker-minted-lease".into(),
            planning: runtime_collab::TaskRunPlanning::RulesOnly,
            execution_budget: None,
            action_context: serde_json::json!({
                "purpose": "Review draft",
                "group": {"id": "task:review", "label": "Review draft", "kind": "task"}
            }),
        }
    }

    #[test]
    fn managed_context_rejects_private_task_text_in_public_attribution() {
        let mut run = task_run();
        assert!(validate_enrolled_context(&run).is_err());
        run.action_context["purpose"] = serde_json::Value::Null;
        run.action_context["group"]["label"] = serde_json::json!(run.task_id);
        assert!(validate_enrolled_context(&run).is_ok());
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
                            continuation: None,
                            retry_step: false,
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
                    CollabRequest::ReportEnrolledTask { request_id, .. } => {
                        Some(CollabEvent::EnrolledTaskReply {
                            request_id: *request_id,
                            result: Ok(()),
                        })
                    }
                    CollabRequest::Presence { .. } => None,
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
                let mut run = task_run();
                run.action_context = serde_json::json!({"purpose":null,"group":{"kind":"task","id":run.task_id,"label":run.task_id}});
                let mut custody = runtime_collab::EnrolledRecipientCustody::new(
                    "agent-1",
                    "launch-1",
                    "epoch-1",
                    "channel-1",
                )
                .unwrap();
                let mut envelope = runtime_collab::EnrolledTaskEnvelope {
                    version: 1,
                    message_id: "message-1".into(),
                    task_id: run.task_id.clone(),
                    run_id: "run-1".into(),
                    launch_id: "launch-1".into(),
                    runtime_epoch: "epoch-1".into(),
                    recipient_agent: "agent-1".into(),
                    channel_lease: "channel-1".into(),
                    history: None,
                    task: run.clone(),
                };
                envelope.task.planning = runtime_collab::TaskRunPlanning::Model;
                let acceptance = custody.accept(&envelope).unwrap().acceptance;
                let mut session = Session {
                    cfg: Config {
                        agent_id: "agent-1".into(),
                        endpoint: None,
                        endpoint_identity: None,
                        app_id: "so.tilo.agent".into(),
                        cluster: Uuid::from_u128(1),
                        record: None,
                        name: "agent-1".into(),
                        pace: Duration::ZERO,
                        once: false,
                        model: false,
                        model_timeout: Duration::ZERO,
                        await_document: Duration::ZERO,
                        settle: Duration::ZERO,
                        permissions: ui::permissions::PermissionContext::agent_granted_at(
                            "agent-1",
                            &[],
                            ui::permissions::KindSet::READ_UNDOABLE,
                        ),
                        content: tilo_content::ContentStore::memory(),
                    },
                    link,
                    chooser: Chooser::new(false, Duration::ZERO),
                    enrolled_custody: managed.then_some(custody),
                    enrolled_active: managed.then_some(acceptance),
                    enrolled_sequence: 0,
                    task_deadline: None,
                    native_task: None,
                    broker_surfaces: Vec::new(),
                    next_broker_request: 1,
                    live: crate::live::LivePublisher::default(),
                    memory: agent_loop::Memory::default(),
                    broker_revision: 0,
                    catalogue_cache: None,
                    turn: None,
                    outstanding: outstanding::Outstanding::default(),
                };
                let action = TaskAction {
                    app: "app".into(),
                    peer_id: 2,
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
                    assert!(
                        requests.iter().any(
                            |request| matches!(request, CollabRequest::ReportEnrolledTask {
                        update: runtime_collab::TaskRunUpdate::Progress { message }, ..
                    } if message == marker)
                        ),
                        "private progress must retain action detail"
                    );
                    if cancelled {
                        assert!(
                            requests.iter().any(
                                |request| matches!(request, CollabRequest::ReportEnrolledTask {
                            update: runtime_collab::TaskRunUpdate::Cancelled { message }, ..
                        } if message == marker)
                            ),
                            "private cancellation must retain action detail"
                        );
                    } else {
                        assert!(
                            requests.iter().any(
                                |request| matches!(request, CollabRequest::ReportEnrolledTask {
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

    mod durable_steps {
        use super::*;
        use std::{cell::RefCell, collections::VecDeque, rc::Rc};

        enum Reply {
            Retry,
            Silent,
            Committed(tilo_actions::InvocationUpdate, serde_json::Value),
            Legacy(tilo_actions::InvocationUpdate),
            Refused(&'static str),
        }
        struct Peer {
            sent: Rc<RefCell<Vec<CollabRequest>>>,
            events: Vec<CollabEvent>,
            replies: VecDeque<Reply>,
            statuses: VecDeque<tilo_actions::InvocationUpdate>,
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
                        let request_id = *request_id;
                        let event = |result, continuation, retry_step| {
                            CollabEvent::TaskActionInvocationResult {
                                request_id,
                                result,
                                continuation,
                                retry_step,
                            }
                        };
                        match self.replies.pop_front().expect("scripted reply") {
                            Reply::Retry => Some(event(Err("pending".into()), None, true)),
                            Reply::Silent => None,
                            Reply::Committed(update, continuation) => Some(event(
                                Ok((0, update)),
                                Some(serde_json::to_vec(&continuation).unwrap()),
                                false,
                            )),
                            Reply::Legacy(update) => Some(event(Ok((9, update)), None, false)),
                            Reply::Refused(error) => Some(event(Err(error.into()), None, false)),
                        }
                    }
                    CollabRequest::RemoteAction {
                        message: RemoteActionMessage::Observe { invocation, .. },
                    } => Some(CollabEvent::RemoteAction {
                        message: RemoteActionMessage::Update {
                            invocation: *invocation,
                            state: Some(tilo_actions::remote::InvocationState::new(
                                *invocation,
                                self.statuses.pop_front().unwrap(),
                            )),
                        },
                    }),
                    CollabRequest::TaskRunCancellation { request_id, .. } => {
                        Some(CollabEvent::TaskRunCancellation {
                            request_id: *request_id,
                            result: Ok(false),
                        })
                    }
                    CollabRequest::ReportEnrolledTask { request_id, .. } => {
                        Some(CollabEvent::EnrolledTaskReply {
                            request_id: *request_id,
                            result: Ok(()),
                        })
                    }
                    CollabRequest::Presence { .. } => None,
                    _ => panic!("unexpected request in step path"),
                };
                self.sent.borrow_mut().push(request);
                if let Some(response) = response {
                    self.events.push(response);
                }
                Ok(())
            }
        }

        fn fixture(
            replies: Vec<Reply>,
            statuses: Vec<tilo_actions::InvocationUpdate>,
        ) -> (
            Session,
            runtime_collab::TaskRunEnvelope,
            Rc<RefCell<Vec<CollabRequest>>>,
        ) {
            let sent = Rc::new(RefCell::new(Vec::new()));
            let link = crate::wire::test_link(Peer {
                sent: sent.clone(),
                events: Vec::new(),
                replies: replies.into(),
                statuses: statuses.into(),
            });
            let mut run = task_run();
            run.action_context = serde_json::json!({"purpose":null,"group":{"kind":"task","id":run.task_id,"label":run.task_id}});
            let mut custody = runtime_collab::EnrolledRecipientCustody::new(
                "agent-1",
                "launch-1",
                "epoch-1",
                "channel-1",
            )
            .unwrap();
            let mut envelope = runtime_collab::EnrolledTaskEnvelope {
                version: 1,
                message_id: "message-1".into(),
                task_id: run.task_id.clone(),
                run_id: "run-1".into(),
                launch_id: "launch-1".into(),
                runtime_epoch: "epoch-1".into(),
                recipient_agent: "agent-1".into(),
                channel_lease: "channel-1".into(),
                history: None,
                task: run.clone(),
            };
            envelope.task.planning = runtime_collab::TaskRunPlanning::Model;
            let acceptance = custody.accept(&envelope).unwrap().acceptance;
            let session = Session {
                cfg: Config {
                    agent_id: "agent-1".into(),
                    endpoint: None,
                    endpoint_identity: None,
                    app_id: "so.tilo.agent".into(),
                    cluster: Uuid::from_u128(1),
                    record: None,
                    name: "agent-1".into(),
                    pace: Duration::ZERO,
                    once: false,
                    model: false,
                    model_timeout: Duration::ZERO,
                    await_document: Duration::ZERO,
                    settle: Duration::ZERO,
                    permissions: ui::permissions::PermissionContext::agent_granted_at(
                        "agent-1",
                        &[],
                        ui::permissions::KindSet::READ_UNDOABLE,
                    ),
                    content: tilo_content::ContentStore::memory(),
                },
                link,
                chooser: Chooser::new(false, Duration::ZERO),
                enrolled_custody: Some(custody),
                enrolled_active: Some(acceptance),
                enrolled_sequence: 0,
                task_deadline: None,
                native_task: None,
                broker_surfaces: Vec::new(),
                next_broker_request: 1,
                live: crate::live::LivePublisher::default(),
                memory: agent_loop::Memory::default(),
                broker_revision: 0,
                catalogue_cache: None,
                turn: None,
                outstanding: outstanding::Outstanding::default(),
            };
            (session, run, sent)
        }

        fn action(name: &str, carries: bool) -> TaskAction {
            TaskAction {
                app: "app".into(),
                peer_id: 2,
                resource: "resource".into(),
                resource_kind: "item".into(),
                resource_label: "Resource".into(),
                action: name.into(),
                arguments: serde_json::Map::new(),
                label: name.into(),
                carries: carries.then(|| tilo_actions::PROMPTED_PARAM.to_owned()),
            }
        }

        fn succeeded(resource: &str) -> tilo_actions::InvocationUpdate {
            tilo_actions::InvocationUpdate::Succeeded {
                invocation: tilo_actions::InvocationId(Uuid::from_u128(71)),
                output: tilo_actions::ActionOutput {
                    value: None,
                    resources: vec![resource.into()],
                },
            }
        }

        fn step_requests(sent: &[CollabRequest]) -> Vec<(u64, Option<u32>, String)> {
            sent.iter()
                .filter_map(|request| match request {
                    CollabRequest::InvokeTaskAction {
                        request_id,
                        step,
                        arguments,
                        ..
                    } => Some((
                        *request_id,
                        *step,
                        serde_json::Value::Object(arguments.clone()).to_string(),
                    )),
                    _ => None,
                })
                .collect()
        }

        /// Write `text` as a declared incremental paragraph and return each
        /// step as `(step, action, part, preview shown before it)`.
        fn write_paragraph(text: &str) -> Vec<(Option<u32>, String, String, String)> {
            let replies = (0..crate::live::MAX_CHUNKS)
                .map(|_| Reply::Committed(succeeded("done"), serde_json::json!({"succeeded": {}})))
                .collect();
            let (mut session, run, sent) = fixture(replies, vec![]);
            session.live.start();
            // Generic names: chunking follows the declared parameter and
            // continuation action, never particular operation ids.
            let mut first = action("start", false);
            first.label = "Start".into();
            first.arguments = with_param(&first.arguments, "body", text);
            let next = action("more", false);
            let paragraph = ChunkedParagraph {
                first: &first,
                next: &next,
                param: "body",
                arguments: &first.arguments,
                text,
                budget: crate::live::MAX_CHUNKS,
            };
            let mut step_index = 2;
            let mut stepped = true;
            session
                .run_chunked_paragraph(&run, 1, &mut step_index, &paragraph, &mut stepped)
                .unwrap()
                .unwrap();
            let mut preview = None;
            let mut steps = Vec::new();
            for request in sent.borrow().iter() {
                match request {
                    CollabRequest::ReportEnrolledTask {
                        update: runtime_collab::TaskRunUpdate::Progress { message },
                        ..
                    } => {
                        let update = crate::live::AgentLiveUpdate::decode(message).unwrap();
                        assert_eq!(update.state, crate::live::LiveState::Applying);
                        assert_eq!(update.label, "Start: Resource");
                        preview = update.preview.map(|preview| preview.text);
                    }
                    CollabRequest::InvokeTaskAction {
                        step,
                        action,
                        arguments,
                        ..
                    } => steps.push((
                        *step,
                        action.clone(),
                        arguments["body"].as_str().unwrap().to_owned(),
                        // The preview on show when the step is sent: an
                        // unchanged preview is not published again.
                        preview.clone().expect("a preview precedes the first step"),
                    )),
                    _ => {}
                }
            }
            assert_eq!(step_index, 2 + steps.len());
            steps
        }

        /// A complete 700-byte paragraph is one durable step through the
        /// planned action, previewed before it lands — not a dozen chunk
        /// round trips imitating typing.
        #[test]
        fn a_complete_paragraph_is_one_step_with_its_preview() {
            let text = "The ocean covers more than seventy percent of the planet! ".repeat(13);
            assert!(text.len() >= 700, "{}", text.len());
            let steps = write_paragraph(&text);
            assert_eq!(
                steps,
                [(
                    Some(2),
                    "start".to_owned(),
                    text.clone(),
                    crate::live::tail(&text, crate::live::MAX_PREVIEW_BYTES).to_owned()
                )],
                "one step carries the whole paragraph, after its preview"
            );
        }

        /// Text past one step's bound continues through the declared
        /// continuation, strictly in order, each step sent while the preview
        /// shows the uncommitted rest, and the parts rejoin exactly.
        #[test]
        fn a_paragraph_past_one_steps_bound_continues_in_order_with_previews() {
            let text = "word ".repeat(6000);
            assert!(text.len() > crate::live::MAX_STEP_TEXT_JSON);
            let steps = write_paragraph(&text);
            assert_eq!(steps.len(), 2, "split only at the step bound: {steps:?}");
            let mut rest = text.clone();
            for (index, (step, action, chunk, preview)) in steps.iter().enumerate() {
                assert_eq!(*step, Some(u32::try_from(2 + index).unwrap()));
                let expected = if index == 0 { "start" } else { "more" };
                assert_eq!(action, expected);
                assert!(chunk.len() <= crate::live::MAX_STEP_TEXT_JSON);
                assert_eq!(
                    preview,
                    crate::live::tail(&rest, crate::live::MAX_PREVIEW_BYTES),
                    "the preview is the uncommitted rest"
                );
                rest = rest[chunk.len()..].to_owned();
            }
            assert!(rest.is_empty());
            assert_eq!(
                steps.iter().map(|step| step.2.as_str()).collect::<String>(),
                text
            );
        }

        /// A pending or unanswered step is re-sent identically, and the next
        /// step is built from the committed continuation, not the reply.
        #[test]
        fn pending_steps_resend_identically_and_hand_off_the_committed_continuation() {
            let committed = serde_json::json!({
                "succeeded": {"resources": ["tilo.content:committed-hash"]}
            });
            let (mut session, run, sent) = fixture(
                vec![
                    Reply::Retry,
                    Reply::Committed(succeeded("tilo.content:in-memory-hash"), committed),
                    Reply::Committed(
                        succeeded("tilo.content:final"),
                        serde_json::json!({"succeeded": {}}),
                    ),
                ],
                vec![],
            );
            let mut stepped = true;
            let first = action("produce", false);
            let handoff = session
                .invoke_task_step(&run, 0, &first, &first.arguments, &mut stepped)
                .unwrap()
                .unwrap();
            assert!(matches!(handoff, Handoff::Committed(_)));
            let second = action("consume", true);
            let arguments = arguments_for_step(
                second.carries.as_deref(),
                &second.arguments,
                handoff.outcome().unwrap().as_ref(),
            )
            .unwrap();
            assert_eq!(
                arguments.get(tilo_actions::PROMPTED_PARAM),
                Some(&serde_json::Value::String("committed-hash".into())),
                "the next step consumes only the committed continuation"
            );
            session
                .invoke_task_step(&run, 1, &second, &arguments, &mut stepped)
                .unwrap()
                .unwrap();
            assert!(stepped);
            let requests = step_requests(&sent.borrow());
            assert_eq!(requests.len(), 3);
            assert_eq!(
                requests[0], requests[1],
                "a pending step is re-sent byte-identically"
            );
            assert_eq!(requests[0].1, Some(0));
            assert_eq!(requests[2].1, Some(1));
            assert_ne!(requests[2].0, requests[0].0);
        }

        #[test]
        fn a_committed_failure_stops_the_task_with_the_committed_detail() {
            let failed = tilo_actions::InvocationUpdate::Failed {
                invocation: tilo_actions::InvocationId(Uuid::from_u128(71)),
                error: tilo_actions::ActionFailure {
                    code: "app.refused".into(),
                    message: "The app refused".into(),
                    details: None,
                    retryable: false,
                },
            };
            let (mut session, run, _) = fixture(
                vec![Reply::Committed(
                    failed,
                    serde_json::json!({"failed": "app.refused"}),
                )],
                vec![],
            );
            let mut stepped = true;
            let first = action("produce", false);
            match session
                .invoke_task_step(&run, 0, &first, &first.arguments, &mut stepped)
                .unwrap()
            {
                Err(TaskActionStop::Failed(message)) => assert_eq!(message, "The app refused"),
                other => panic!("committed failure lost: {other:?}"),
            }
        }

        /// A host without a step ledger refuses before binding, so the
        /// non-step invocation path may run; a stepped reply without a
        /// committed continuation is a failure, never a silent fallback.
        #[test]
        fn hosts_without_steps_keep_the_unstepped_path() {
            let accepted = tilo_actions::InvocationUpdate::Accepted {
                invocation: tilo_actions::InvocationId(Uuid::from_u128(71)),
            };
            let (mut session, run, sent) = fixture(vec![Reply::Legacy(accepted)], vec![]);
            let mut stepped = true;
            let first = action("produce", false);
            assert!(matches!(
                session
                    .invoke_task_step(&run, 0, &first, &first.arguments, &mut stepped)
                    .unwrap(),
                Err(TaskActionStop::Failed(_))
            ));
            assert!(stepped);
            assert_eq!(step_requests(&sent.borrow()).len(), 1, "never re-sent");

            let (mut session, run, sent) = fixture(
                vec![
                    Reply::Refused("remote exception: enrolled task steps unavailable"),
                    Reply::Legacy(succeeded("tilo.content:legacy")),
                ],
                vec![],
            );
            let mut stepped = true;
            session
                .invoke_task_step(&run, 0, &first, &first.arguments, &mut stepped)
                .unwrap()
                .unwrap();
            assert!(!stepped);
            let requests = step_requests(&sent.borrow());
            assert_eq!(
                requests.iter().map(|request| request.1).collect::<Vec<_>>(),
                vec![Some(0), None]
            );

            let (mut session, run, sent) = fixture(
                vec![Reply::Refused("task action is no longer live")],
                vec![],
            );
            let mut stepped = true;
            assert!(matches!(
                session
                    .invoke_task_step(&run, 0, &first, &first.arguments, &mut stepped)
                    .unwrap(),
                Err(TaskActionStop::Failed(_))
            ));
            assert!(stepped);
            assert_eq!(
                step_requests(&sent.borrow()).len(),
                1,
                "a refusal is not retried"
            );
        }

        #[test]
        fn an_unanswered_step_is_resent_under_the_same_identity() {
            let (mut session, run, sent) = fixture(
                vec![
                    Reply::Silent,
                    Reply::Committed(
                        succeeded("tilo.content:x"),
                        serde_json::json!({"succeeded": {}}),
                    ),
                ],
                vec![],
            );
            let mut stepped = true;
            let first = action("produce", false);
            session
                .invoke_task_step(&run, 0, &first, &first.arguments, &mut stepped)
                .unwrap()
                .unwrap();
            let requests = step_requests(&sent.borrow());
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0], requests[1]);
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
    #[allow(
        clippy::too_many_lines,
        reason = "each case keeps its original fixture and assertions"
    )]
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
