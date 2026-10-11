//! One agent session: join the room, hold the replica, act on instructions.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use ui::time::{Duration, Instant};

use anyhow::{Context, Result};
use tilo_actions::remote::RemoteActionMessage;
use ui::collaboration::{
    CertifiedEndpointIdentity, CollabEvent, CollabRequest, PresencePayload, RouteSpec,
};
use ui::permissions::PermissionContext;
use ui::presence::{PathSegment, PresenceKey};
use uuid::Uuid;
use word_processor::actions::{self as wp, RegionKind, RichTextAction};

use crate::apply::Editor;
use crate::model::{Chooser, Route};
use crate::plan::{self, Block, ConfirmationEvidence, Instruction, Plan};
use crate::providers::default_selection;
use crate::surface::{self, AppContract};
use crate::wire::Link;

mod agent_loop;
mod chat;
mod native_task;
mod outstanding;
use chat::ChatRun;

/// The default [`Config::pace`], in milliseconds: none. Demos that want the
/// caret to be watched pass `--pace-ms`.
pub const DEFAULT_PACE_MS: u64 = 0;

/// How long a fetched catalogue is trusted before acting fetches it again.
const CATALOGUE_FRESH_FOR: Duration = Duration::from_secs(1);
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
    /// This action's sole content argument comes from the immediately
    /// preceding action result, never from model-authored JSON.
    carries: bool,
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
    carries: bool,
    planned: &serde_json::Map<String, serde_json::Value>,
    previous: Option<&StepOutcome>,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    if !carries {
        return Ok(planned.clone());
    }
    let hash = previous
        .ok_or_else(|| "a content-consuming action has no preceding action result".to_string())?
        .content_hash()?;
    let mut arguments = planned.clone();
    // `prompted_content` always projects its one input under this shared
    // parameter name. Replace any model-authored value: the descriptor says
    // the value is carried, so only the preceding application may supply it.
    arguments.insert(
        tilo_actions::PROMPTED_PARAM.to_string(),
        serde_json::Value::String(hash.to_string()),
    );
    Ok(arguments)
}

fn dispatched_action_key(
    id: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> String {
    format!("{id}\0{}", serde_json::Value::Object(arguments.clone()))
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
        carries: offer.wants_content(),
    })
}

/// Durable evidence collected while one addressed instruction is running.
/// The task record is written only after the agent reaches a terminal reply
/// or completed plan, so a partially dispatched task cannot masquerade as a
/// finished one.
#[derive(Clone, Debug)]
struct ActiveRun {
    instruction: String,
    issued_by: Option<String>,
    decision: String,
    group_id: String,
    steps: Vec<ui::RunStep>,
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
    editor: Option<Editor>,
    /// Every other application's document that has turned up in this room,
    /// keyed by namespace. The room is not one document — a peer that
    /// announced no app id reaches every subscriber — so what an app other
    /// than [`Self::editor`]'s exports has to live somewhere before
    /// [`Self::offers`] can survey it or [`Self::perform`] can act on it.
    peers: BTreeMap<&'static str, Editor>,
    /// Instruction blocks already acted on, so a slow peer re-sending the
    /// same state does not make the agent do the work twice.
    handled: HashSet<String>,
    /// A paragraph currently addressed to the agent, and the text it held
    /// the last time this was checked — the [`Config::settle`] debounce.
    /// Cleared once the paragraph is handled or stops naming the agent.
    watching: HashMap<String, (String, Instant)>,
    /// Addresses already performed in the plan currently running.
    ///
    /// A stalled clause can be asked again and come back as a whole plan of
    /// its own, and that plan can re-name a step this turn already
    /// dispatched — nothing upstream of dispatch is what stops that
    /// happening twice. Cleared at the start of every [`Session::run_plan`],
    /// never across turns: a later turn re-performing the same address is
    /// an ordinary repeat instruction, not re-entrancy.
    dispatched_this_turn: HashSet<String>,
    /// Decides which exported action answers an instruction.
    chooser: Chooser,
    enrolled_custody: Option<runtime_collab::EnrolledRecipientCustody>,
    enrolled_active: Option<runtime_collab::EnrolledTaskAcceptance>,
    enrolled_sequence: u64,
    /// Monotonic deadline from an explicit broker-minted owner task policy.
    task_deadline: Option<Instant>,
    native_task: Option<native_task::NativeTaskState>,
    /// When the agent last asked the room for the document. It asks again
    /// until it has one: an app that was still opening its file when the
    /// agent arrived never saw the first ask, and the agent would then wait
    /// for a reply that is never coming. On a loaded machine that is the
    /// difference between the beat happening and the beat hanging.
    asked_at: Instant,
    /// How the last instruction was decided. The record that keeps a run on
    /// the fallback from being indistinguishable from a live one afterwards.
    last_route: Option<Route>,
    /// Complete, permission-filtered live action catalogue the session's
    /// action host offers this agent. This is the planner's application
    /// catalogue; local document contracts remain only for the agent's own
    /// editorial state/tools.
    broker_catalogue: tilo_actions::remote::CataloguePage,
    /// When [`Self::broker_catalogue`] was last fetched.
    catalogue_fetched: Option<Instant>,
    /// The broker's pushed, permission-filtered live action directory. The
    /// task loop and Chat runner survey this; document performs go through
    /// [`Self::broker_catalogue`] and the action host.
    broker_surfaces: Vec<ui::collaboration::BrokerActionSurface>,
    next_broker_request: u64,
    active_run: Option<ActiveRun>,
    chat_run: Option<ChatRun>,
    chat_pending: bool,
    chat_catalogue_received: bool,
    last_chat_action: Option<crate::listen::LastAction>,
    /// First accepted forwarding frame for each message. Its certified key
    /// authenticates the frame, never the inner Message.author claim.
    chat_sources: HashMap<String, (String, collab_types::Ed25519PublicKey)>,
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

/// How often the agent asks again while it still has no document.
const REASK_EVERY: Duration = Duration::from_millis(750);

/// Ask the broker to select one current holder of this exact route. Safe to
/// repeat while no holder is available; unlike presence fan-out, one request
/// produces at most one full snapshot.
fn ask_for_document(link: &mut Link) -> Result<()> {
    link.send(&CollabRequest::RequestDocumentSnapshot)
}

fn is_chat_app_id(app_id: &str) -> bool {
    app_id == "so.tilo.chat"
}

/// How the word processor keys a caret: the block's own presence key.
fn block_key(block_id: &str) -> PresenceKey {
    PresenceKey::Widget {
        path: vec![
            PathSegment::Field("wp_block".to_string()),
            PathSegment::Field(block_id.to_string()),
        ],
    }
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
        if is_chat_app_id(&cfg.app_id) && cfg.record.is_some() {
            anyhow::bail!("canonical Chat requires the package-primary route");
        }
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
            editor: None,
            peers: BTreeMap::new(),
            handled: HashSet::new(),
            watching: HashMap::new(),
            dispatched_this_turn: HashSet::new(),
            chooser,
            enrolled_custody,
            enrolled_active: None,
            enrolled_sequence: 0,
            task_deadline: None,
            native_task: None,
            last_route: None,
            asked_at: ui::time::now(),
            broker_catalogue: tilo_actions::remote::CataloguePage::default(),
            catalogue_fetched: None,
            broker_surfaces: Vec::new(),
            next_broker_request: 1,
            active_run: None,
            chat_run: None,
            chat_pending: false,
            chat_catalogue_received: false,
            last_chat_action: None,
            chat_sources: HashMap::new(),
            live: crate::live::LivePublisher::default(),
            memory: agent_loop::Memory::default(),
            broker_revision: 0,
            catalogue_cache: None,
            turn: None,
            outstanding: outstanding::Outstanding::default(),
        })
    }

    fn has_primary_document(&self) -> bool {
        if is_chat_app_id(&self.cfg.app_id) {
            self.peers.contains_key("chat")
        } else {
            self.editor.is_some()
        }
    }

    /// How the last instruction was decided, for a test or an operator
    /// checking after the fact whether a run was live.
    ///
    /// `None` after a request that read as several things to do: no single
    /// model call decided a sequence, so there is no one [`Route`] to name
    /// without claiming a call happened that did not.
    pub fn last_route(&self) -> Option<&Route> {
        self.last_route.as_ref()
    }

    /// Run until the broker goes away — or, with `once`, until the first
    /// instruction has been carried out.
    ///
    /// If nobody ever answers with a document, give up after
    /// `await_document` and say which address went unanswered. Waiting
    /// forever was the previous behaviour and it is the wrong one: the
    /// commonest cause is a route that does not match, the broker drops
    /// a mismatched frame silently by design, and the symptom is an
    /// agent sitting at 0% forever while every log line says it joined
    /// fine. A harness then reports "the agent did not finish", which
    /// names the one component that was working.
    ///
    /// # Errors
    ///
    /// When no participant answers with the document within
    /// `await_document` (see `Self::unanswered` for the message), or when
    /// a turn's own I/O — receiving from the broker, decoding a snapshot,
    /// applying an edit — fails.
    pub fn run(&mut self) -> Result<()> {
        let joined = ui::time::now();
        loop {
            if self.step(Duration::from_millis(100))? && self.cfg.once {
                self.link.complete_publications(None)?;
                return self.link.flush_documents();
            }
            if !self.has_primary_document()
                && !self.cfg.await_document.is_zero()
                && joined.elapsed() >= self.cfg.await_document
            {
                anyhow::bail!("{}", self.unanswered());
            }
        }
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
                arguments_for_step(action.carries, &action.arguments, previous.as_ref())
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
    /// parts only past one step's bound): its own descriptor declares an
    /// incremental Text parameter ([`crate::live::INCREMENTAL_TEXT_TAG`])
    /// whose value is longer than [`crate::live::CHUNK_MIN_TEXT`], and the
    /// declared continuation action is published on the same resource with
    /// that Text parameter. Only enrolled tasks chunk. Returns the parameter,
    /// its text, and the continuation step.
    fn chunkable(
        &self,
        action: &TaskAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<(String, String, TaskAction)> {
        if self.enrolled_active.is_none() || action.carries {
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

    /// The incremental Text parameter `offer` declares
    /// ([`crate::live::INCREMENTAL_TEXT_TAG`]) and its continuation step, when
    /// that continuation is published on the same resource with the same
    /// Text parameter.
    fn continuation_of(&self, offer: &surface::Offer) -> Option<(String, TaskAction)> {
        let peer = offer.broker_peer_id?;
        let (param, next) = crate::live::incremental_text(&offer.descriptor.contract.tags)?;
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
        sample.insert(param.to_owned(), serde_json::Value::String("…".into()));
        let next = task_action_from_offer(&continuation, sample).ok()?;
        Some((param.to_owned(), next))
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
                kind: tilo_actions::InteractionActorKind::Agent,
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
                Some(CollabEvent::TaskActionInvocationResult { .. }) | None => {}
                Some(CollabEvent::ActionCatalogue { surfaces }) => {
                    self.set_broker_surfaces(surfaces);
                }
                Some(event @ CollabEvent::Document { .. }) => {
                    let _ = self.absorb_document_event(&event)?;
                }
                Some(event) => deferred.push(event),
            }
        }
    }

    /// Why no document arrived, in terms of the thing that decides it.
    ///
    /// The broker's fan-out matches `(app, room, record)` and the record
    /// is the innermost test — it fails closed both ways, and "no
    /// record" is a value like any other rather than a wildcard. So an
    /// agent told nothing about the record does not join an untitled
    /// conversation *and* a saved one: it joins the untitled one only,
    /// and a word processor with a file open is announcing
    /// `file:<path>` and cannot hear it.
    fn unanswered(&self) -> String {
        let waited = self.cfg.await_document.as_secs();
        let addressed = match self.cfg.record.as_deref() {
            Some(r) => format!("record {r:?}"),
            None => "no record (an untitled document)".to_string(),
        };
        format!(
            "no participant answered with a document in {waited}s. Announced \
             app={:?} room={} and {addressed}. The broker matches all three \
             and drops what does not match, so if the app has a file open \
             its record is `file:<path>` and this agent must be given the \
             same one with --record.",
            self.cfg.app_id, self.cfg.cluster,
        )
    }

    /// Run until `deadline`, then return whether an instruction was
    /// carried out. For tests.
    ///
    /// Every edit made is published before this returns, so a caller that
    /// then drops the session cannot discard the last of them in transit.
    ///
    /// # Errors
    ///
    /// Whenever a turn's own I/O fails — see [`Self::run`] — or an edit
    /// could not be published.
    pub fn run_until(&mut self, deadline: Instant) -> Result<bool> {
        let mut acted = false;
        while ui::time::now() < deadline {
            acted |= self.step(Duration::from_millis(50))?;
            if acted && self.cfg.once {
                break;
            }
        }
        self.link.complete_publications(Some(deadline))?;
        self.link.flush_documents()?;
        Ok(acted)
    }

    /// One turn: absorb whatever the room sent, then act if the document
    /// asks. Returns whether an instruction was carried out.
    fn step(&mut self, wait: Duration) -> Result<bool> {
        let mut changed = false;
        // Still empty-handed: ask again. An app that was still opening its
        // file when the agent arrived never saw the first ask, and nothing
        // else would ever prompt a reply.
        if !self.has_primary_document() && self.asked_at.elapsed() >= REASK_EVERY {
            ask_for_document(&mut self.link)?;
            self.asked_at = ui::time::now();
            tracing::debug!("asked the room for the document again");
        }
        match self.link.recv(wait)? {
            Some(event @ CollabEvent::Document { .. }) => {
                changed = self.absorb_document_event(&event)?;
            }
            Some(CollabEvent::DocumentSnapshotRequested { route }) => {
                if route.app_id == self.cfg.app_id && route.record == self.cfg.record {
                    if let Some(editor) = self.editor.as_mut() {
                        self.link
                            .send(&CollabRequest::document_snapshot(editor.full()))?;
                    }
                } else if let Some(peer) = self
                    .peers
                    .values_mut()
                    .find(|peer| peer.route_app_id() == route.app_id)
                {
                    let full = peer.full();
                    self.link
                        .send_to_route(route, &CollabRequest::document_snapshot(full))?;
                }
            }
            Some(CollabEvent::ActionCatalogue { surfaces }) => {
                self.set_broker_surfaces(surfaces);
                self.chat_catalogue_received = true;
                changed = is_chat_app_id(&self.cfg.app_id);
                tracing::debug!(
                    actions = self
                        .broker_surfaces
                        .iter()
                        .map(|s| s.actions.len())
                        .sum::<usize>(),
                    "updated broker action catalogue"
                );
            }
            Some(_) | None => {}
        }
        if !changed && !self.chat_pending {
            return Ok(false);
        }
        if is_chat_app_id(&self.cfg.app_id) {
            let acted = self.act_chat()?;
            // Completion can leave another summons in the same snapshot.
            // Waiting parks until new state arrives, without a polling loop.
            self.chat_pending = acted;
            Ok(acted)
        } else {
            self.act()
        }
    }

    /// Take a peer's whole-document frame, if it is ours to take.
    ///
    /// A room is not one document. Every participant answers an arrival
    /// with its own state, and the broker hands a frame to any
    /// subscriber whose address it matches — which, for a participant
    /// that announced no app id, is everyone in the room. So the agent
    /// is offered the file manager's state alongside the page it was
    /// invited to, and nothing in the frame says which is which.
    ///
    /// Given a `--record` the address settles it and only one arrives.
    /// Without one, this does what it can from the content: take the
    /// first thing that is a document at all, and if a later one turns
    /// out to be a *different* document — established by its refusing to
    /// merge — prefer whichever of the two is actually asking us for
    /// something.
    ///
    /// Returns whether the agent's replica moved.
    fn absorb_snapshot(&mut self, artifact: collab_types::ArtifactId, data: &[u8]) -> Result<bool> {
        let cluster = self.cfg.cluster.to_string();
        let primary = surface::for_app_id(&self.cfg.app_id);
        let on_primary_route = artifact
            == collab_types::document_artifact_id(
                &self.cfg.app_id,
                &cluster,
                self.cfg.record.as_deref().unwrap_or_default(),
            );
        let Some(ed) = self.editor.as_mut() else {
            let Some(contract) = primary else {
                return self.absorb_peer(artifact, data);
            };
            // Without a record, the page is whichever document arrives
            // first; with one, the route settles it.
            let ours = on_primary_route
                || (self.cfg.record.is_none() && Editor::looks_like_a_document(data));
            if !ours {
                // Not a page this agent authors into — but it may still be
                // another application's room state, which belongs in
                // `peers` rather than being dropped outright.
                return self.absorb_peer(artifact, data);
            }
            self.editor = Some(Editor::from_snapshot(
                contract,
                data,
                &self.cfg.agent_id,
                self.cfg.permissions.clone(),
            )?);
            tracing::info!(bytes = data.len(), "received the document");
            return Ok(true);
        };
        if ed.merge_full(data) {
            return Ok(true);
        }
        // It did not merge, so it is not this document. Worth having only
        // if it is the one addressed to us and what we hold is not.
        let ours_asks = ed
            .snapshot()
            .is_ok_and(|s| plan::find_addressed(&plan::blocks(&s), &self.cfg.name).is_some());
        if !ours_asks && Editor::is_addressed_to(data, &self.cfg.name) {
            ed.replace_with(data)?;
            tracing::info!(
                bytes = data.len(),
                "took a different document: this is the one addressed to {}",
                self.cfg.name
            );
            return Ok(true);
        }
        // Not the page we were invited to and not addressed to us — but
        // the room holds more than the document we were invited to, and an
        // instruction may name an operation that belongs to whatever else
        // is here.
        self.absorb_peer(artifact, data)
    }

    /// Take a peer's whole-document frame as a *second* application's
    /// state — one this session was not invited to, but whose operations
    /// [`Self::offers`] can still discover and [`Self::perform`] can still
    /// dispatch. Which app it is comes from the authenticated route it
    /// arrived on.
    fn absorb_peer(&mut self, artifact: collab_types::ArtifactId, data: &[u8]) -> Result<bool> {
        let Some(contract) = surface::for_artifact(&self.cfg.cluster.to_string(), artifact) else {
            tracing::debug!(
                bytes = data.len(),
                "ignored a peer's state: no app the agent speaks to wrote it"
            );
            return Ok(false);
        };
        if let Some(peer) = self.peers.get_mut(contract.namespace) {
            return Ok(peer.merge_full(data));
        }
        let peer = Editor::peer_from_snapshot_on_route(
            contract,
            contract.app_id(),
            data,
            &self.cfg.agent_id,
            self.cfg.permissions.clone(),
        )?;
        tracing::info!(
            app = contract.namespace,
            bytes = data.len(),
            "a second application's document joined the room"
        );
        self.peers.insert(contract.namespace, peer);
        Ok(true)
    }

    /// Every document this session holds, primary first, as the pairs
    /// [`surface::offers`] and [`surface::facts`] are built from.
    fn held(&self) -> Vec<(&'static AppContract, &automerge::AutoCommit)> {
        let mut out = Vec::new();
        if let Some(ed) = self.editor.as_ref() {
            out.push((ed.contract(), ed.doc()));
        }
        for peer in self.peers.values() {
            out.push((peer.contract(), peer.doc()));
        }
        out
    }

    /// Prefer broker-routable offers, while retaining the known-document
    /// catalogue until the asynchronous broker snapshot has advertised the
    /// same address. This prevents an instruction arriving with the document
    /// from being permanently refused merely because its catalogue event is
    /// one frame behind it.
    fn offers(
        &self,
        held: &[(&'static AppContract, &automerge::AutoCommit)],
    ) -> Vec<surface::Offer> {
        if is_chat_app_id(&self.cfg.app_id) {
            // Canonical Chat acts only through package-primary routes in its
            // own room, as the broker's pushed directory names them.
            let mut offers = Self::document_broker_offers(
                self.broker_surfaces.as_slice(),
                &self.cfg.permissions,
            );
            offers.retain(|offer| {
                self.broker_surfaces.iter().any(|surface| {
                    Some(surface.peer_id) == offer.broker_peer_id
                        && surface.cluster_id == self.cfg.cluster.to_string()
                        && surface.record_id.is_empty()
                })
            });
            return offers;
        }
        let mut offers =
            Self::document_broker_offers(&self.broker_catalogue, &self.cfg.permissions);
        // An app's own live publication is the whole of what it offers:
        // beside its document's tree it carries what only its window knows
        // (an edit at the caret). The tree read off the held replica stands
        // in only for an app whose publication has not arrived yet.
        let live: HashSet<String> = offers.iter().map(|offer| offer.app.clone()).collect();
        let mut local = surface::offers(held, &offers, &self.cfg.permissions);
        local.retain(|offer| {
            surface::for_namespace(&offer.app)
                .is_none_or(|contract| !live.contains(contract.app_id()))
                && !offers
                    .iter()
                    .any(|broker| broker.address() == offer.address())
        });
        offers.extend(local);
        offers
    }

    /// Broker authorization is necessary but the invitation carried by this
    /// document session remains an independent upper bound. Task leases use
    /// the broker catalogue directly; document instructions use the
    /// intersection so a stale or narrower launch grant cannot be widened by
    /// a broader workspace catalogue.
    fn document_broker_offers<C: surface::BrokerCatalogue + ?Sized>(
        catalogue: &C,
        permissions: &PermissionContext,
    ) -> Vec<surface::Offer> {
        surface::broker_offers(catalogue)
            .into_iter()
            .filter(|offer| {
                surface::permitted_descriptor(permissions, &offer.app, &offer.descriptor)
            })
            .collect()
    }

    /// Perform the discovered operation `id` names, through the broker, as
    /// the application that publishes it. `Ok(Err(_))` is a refusal in words
    /// a person reads; `Err(_)` is this session's own I/O failing.
    ///
    /// Only a live broker publication is performed: the owning application
    /// applies it, exactly as it applies a person's palette invocation. An
    /// operation the agent can only see in its own replica — not published
    /// by the application right now — is refused, never applied to the
    /// replica behind the application's back.
    fn perform(
        &mut self,
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmation: &ConfirmationEvidence,
    ) -> Result<Result<StepOutcome, String>> {
        if let Some(offer) = self.wait_for_broker_offer(id)? {
            return self.perform_broker(id, &offer, arguments, confirmation);
        }
        Ok(Err(
            "The owning application is not offering this action through the broker".into(),
        ))
    }

    /// Find the host offer `id` names, fetching a fresh catalogue once if
    /// the one held does not name it: an app publishes its actions before
    /// its document reaches this agent, but the agent's catalogue may be
    /// older than both.
    fn wait_for_broker_offer(&mut self, id: &str) -> Result<Option<surface::Offer>> {
        // A broker-prefixed id pins an exact live publisher. Only a local
        // compatibility offer may be upgraded to the matching broker route;
        // never silently retarget a vanished broker selection to another app.
        let wanted = (!id.starts_with("broker:"))
            .then(|| surface::decode_address(id))
            .flatten();
        let permissions = self.cfg.permissions.clone();
        let find = |offers: Vec<surface::Offer>| {
            surface::find(&offers, id).cloned().or_else(|| {
                wanted
                    .as_ref()
                    .and_then(|address| offers.into_iter().find(|offer| offer.address() == address))
            })
        };
        if let Some(offer) = find(Self::document_broker_offers(
            &self.broker_catalogue,
            &permissions,
        )) {
            return Ok(Some(offer));
        }
        if let Some(offer) = find(Self::document_broker_offers(
            self.broker_surfaces.as_slice(),
            &permissions,
        )) {
            return Ok(Some(offer));
        }
        self.refresh_catalogue()?;
        if let Some(offer) = find(Self::document_broker_offers(
            &self.broker_catalogue,
            &permissions,
        )) {
            return Ok(Some(offer));
        }
        // The broker's pushed directory travels on its own stream and may
        // be one frame behind the document that named the action.
        let deadline = self.task_wait_deadline(TASK_CATALOGUE_WAIT);
        let mut deferred = Vec::new();
        let mut found = None;
        while ui::time::now() < deadline {
            let remaining = deadline.saturating_duration_since(ui::time::now());
            match self.link.recv(remaining.min(Duration::from_millis(100)))? {
                Some(CollabEvent::ActionCatalogue { surfaces }) => {
                    self.set_broker_surfaces(surfaces);
                    if let Some(offer) = find(Self::document_broker_offers(
                        self.broker_surfaces.as_slice(),
                        &permissions,
                    )) {
                        found = Some(offer);
                        break;
                    }
                }
                Some(event) => deferred.push(event),
                None => {}
            }
        }
        self.link.defer(deferred);
        Ok(found)
    }

    /// Fetch the catalogue unless the one held is fresh and already names
    /// every app whose document the agent holds: an app publishes its
    /// actions before its document reaches the agent, so a document from an
    /// app the catalogue does not name means the catalogue is older than it.
    fn refresh_stale_catalogue(&mut self) -> Result<()> {
        let fresh = self
            .catalogue_fetched
            .is_some_and(|at| at.elapsed() < CATALOGUE_FRESH_FOR);
        let names_every_held_app = self.held().iter().all(|(contract, _)| {
            self.broker_catalogue
                .rows
                .iter()
                .any(|row| row.app_id == contract.app_id())
        });
        if fresh && names_every_held_app {
            return Ok(());
        }
        self.refresh_catalogue()
    }

    /// Ask the session's action host what it offers this agent now, page by
    /// page, into one catalogue.
    fn refresh_catalogue(&mut self) -> Result<()> {
        let mut gathered = tilo_actions::remote::CataloguePage::default();
        let mut cursor = Vec::new();
        loop {
            let correlation = self.next_request_id();
            self.link.send(&CollabRequest::RemoteAction {
                message: RemoteActionMessage::CatalogueRequest {
                    correlation,
                    query: tilo_actions::remote::CatalogueQuery {
                        cursor: std::mem::take(&mut cursor),
                        revision: (!gathered.revision.is_empty())
                            .then(|| gathered.revision.clone()),
                        max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                        ..tilo_actions::remote::CatalogueQuery::default()
                    },
                },
            })?;
            let page = match self.wait_for_remote_answer(|message| {
                matches!(
                    message,
                    RemoteActionMessage::CatalogueResponse { correlation: answered, .. }
                        if *answered == correlation
                )
            })? {
                Some(RemoteActionMessage::CatalogueResponse {
                    result: Ok(page), ..
                }) => page,
                Some(RemoteActionMessage::CatalogueResponse {
                    result: Err(error), ..
                }) => {
                    tracing::warn!(%error, "the action host refused the catalogue");
                    return Ok(());
                }
                _ => {
                    tracing::warn!("the action host did not answer the catalogue request");
                    return Ok(());
                }
            };
            if page.stale_cursor {
                // The offer changed mid-walk; start again from the top.
                gathered = tilo_actions::remote::CataloguePage::default();
                continue;
            }
            gathered.revision = page.revision;
            gathered.rows.extend(page.rows);
            if gathered.providers.is_empty() {
                gathered.providers = page.providers;
            }
            if page.exhausted || page.next_cursor.is_empty() {
                break;
            }
            cursor = page.next_cursor;
        }
        tracing::debug!(
            actions = gathered.rows.len(),
            "fetched the action catalogue"
        );
        self.broker_catalogue = gathered;
        self.catalogue_fetched = Some(ui::time::now());
        Ok(())
    }

    /// The provider token the host issued for `offer`.
    fn provider_for(&self, offer: &surface::Offer) -> Option<String> {
        match offer.broker_peer_id? {
            0 => self
                .broker_catalogue
                .rows
                .iter()
                .find(|row| {
                    tilo_actions::remote::workspace_provider_room(&row.provider).is_some()
                        && row
                            .action
                            .as_ref()
                            .is_some_and(|action| action.address == offer.descriptor.address)
                })
                .map(|row| row.provider.clone()),
            peer => Some(peer.to_string()),
        }
    }

    /// Observe `invocation` until it is terminal or `deadline` passes,
    /// starting from `update`. Between observations the session waits on the
    /// link rather than sleeping: an update the host pushes is taken as it
    /// arrives, and the room's own change (most often this action's effect)
    /// asks again at once. The pause grows from [`FOLLOW_FIRST_WAIT`], so a
    /// quick action is seen settled in milliseconds.
    fn follow_invocation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        mut update: tilo_actions::InvocationUpdate,
        deadline: Instant,
    ) -> Result<Result<tilo_actions::InvocationUpdate, String>> {
        let mut wait = FOLLOW_FIRST_WAIT;
        while !update.is_terminal() && ui::time::now() < deadline {
            let left = deadline.saturating_duration_since(ui::time::now());
            if let Some(pushed) = self.await_invocation_news(invocation, wait.min(left))? {
                update = pushed;
                continue;
            }
            wait = (wait * 2).min(FOLLOW_MOST_WAIT);
            match self.observe(invocation)? {
                Ok(latest) => update = latest,
                Err(error) => return Ok(Err(error)),
            }
        }
        Ok(Ok(update))
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
                Some(event @ CollabEvent::Document { .. }) => {
                    let _ = self.absorb_document_event(&event)?;
                    break;
                }
                Some(CollabEvent::ActionCatalogue { surfaces }) => {
                    self.set_broker_surfaces(surfaces);
                }
                Some(event) => deferred.push(event),
                None => break,
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

    #[allow(
        clippy::too_many_lines,
        reason = "one broker invocation keeps admission, content handoff, observation and the replica check together"
    )]
    fn perform_broker(
        &mut self,
        id: &str,
        offer: &surface::Offer,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmation: &ConfirmationEvidence,
    ) -> Result<Result<StepOutcome, String>> {
        let Some(provider) = self.provider_for(offer) else {
            return Ok(Err(
                "that operation is local-only and cannot be brokered".to_string()
            ));
        };
        let descriptor = offer.descriptor.clone();
        if !confirmation.satisfies(&descriptor.contract.confirmation) {
            return Ok(Err("that action requires explicit confirmation".to_string()));
        }
        if let Err(error) = tilo_actions::ActionArgs::bind_json(&descriptor.params, arguments) {
            return Ok(Err(format!("I couldn't do that - {error}")));
        }
        let peer_id = offer.broker_peer_id;
        if self.chat_run.is_some() && descriptor.kind != tilo_actions::OperationKind::Document {
            return Ok(Err("This Chat runner cannot admit external effects with durable delegated identity yet".into()));
        }
        if self.chat_run.is_some()
            && !self.broker_surfaces.iter().any(|surface| {
                Some(surface.peer_id) == peer_id
                    && surface.cluster_id == self.cfg.cluster.to_string()
                    && surface.record_id.is_empty()
            })
        {
            return Ok(Err(
                "This Chat runner requires the owning application's package-primary route".into(),
            ));
        }
        if let Some(run) = &mut self.chat_run {
            run.observed_mutation = false;
            run.target_artifact = self
                .broker_surfaces
                .iter()
                .find(|surface| Some(surface.peer_id) == peer_id)
                .map(|surface| {
                    collab_types::document_artifact_id(
                        &surface.app_id,
                        &surface.cluster_id,
                        &surface.record_id,
                    )
                });
        }
        let resource = descriptor.address.resource.clone();
        let action_label = descriptor.action_label.clone();
        let invocation = tilo_actions::InvocationId::new();
        self.link.send(&CollabRequest::RemoteAction {
            message: RemoteActionMessage::Invoke(tilo_actions::remote::InvokeRequest {
                on_behalf_of: None,
                invocation,
                target: tilo_actions::remote::RemoteActionTarget {
                    provider,
                    address: descriptor.address,
                },
                argument: tilo_actions::remote::InvokeArgument::Json(arguments.clone()),
                context: self.task_context(),
                confirmation: match confirmation {
                    ConfirmationEvidence::None => None,
                    ConfirmationEvidence::Confirm => {
                        Some(tilo_actions::remote::Confirmation::Confirmed)
                    }
                    ConfirmationEvidence::Phrase(phrase) => {
                        Some(tilo_actions::remote::Confirmation::Phrase(phrase.clone()))
                    }
                },
                revision: None,
                wait_ms: None,
            }),
        })?;
        self.chat_dispatched(&resource, &action_label)?;
        let Some(RemoteActionMessage::Receipt(receipt)) =
            self.wait_for_remote_answer(|message| {
                matches!(
                    message,
                    RemoteActionMessage::Receipt(receipt) if receipt.invocation == invocation
                )
            })?
        else {
            return Ok(Err(
                "I couldn't do that - the action host did not answer".to_string()
            ));
        };
        let update = match receipt.status {
            tilo_actions::remote::ReceiptStatus::Rejected { error } => {
                return Ok(Err(format!("I couldn't do that - {error}")));
            }
            tilo_actions::remote::ReceiptStatus::Accepted => {
                tilo_actions::InvocationUpdate::Accepted { invocation }
            }
            tilo_actions::remote::ReceiptStatus::Completed { output } => {
                tilo_actions::InvocationUpdate::Succeeded { invocation, output }
            }
            tilo_actions::remote::ReceiptStatus::Failed { error } => {
                tilo_actions::InvocationUpdate::Failed { invocation, error }
            }
            tilo_actions::remote::ReceiptStatus::Cancelled { message } => {
                tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message,
                }
            }
        };
        let update = match self.follow_invocation(
            invocation,
            update,
            ui::time::now() + Duration::from_secs(30),
        )? {
            Ok(update) => update,
            Err(error) => return Ok(Err(format!("I couldn't observe that action - {error}"))),
        };
        if let Some(run) = &mut self.chat_run
            && update.is_terminal()
        {
            // Cancellation acknowledges stopping, not an untouched document
            // or rollback. Any observed mutation also survives an unsuccessful
            // terminal receipt; neither case can truthfully become Refused.
            run.uncertain = !matches!(&update, tilo_actions::InvocationUpdate::Succeeded { .. })
                && (run.observed_mutation
                    || matches!(&update, tilo_actions::InvocationUpdate::Cancelled { .. })
                    || matches!(&update, tilo_actions::InvocationUpdate::Failed { error, .. }
                        if error.code == "action_outcome_unknown" ||
                        (error.code == "action_failed" && error.message == "application disconnected before completing the action")));
        }
        match update {
            tilo_actions::InvocationUpdate::Failed { error, .. } => {
                Ok(Err(format!("I couldn't do that - {}", error.message)))
            }
            tilo_actions::InvocationUpdate::Cancelled { message, .. } => {
                Ok(Err(message.unwrap_or_else(|| {
                    "that action was cancelled".to_string()
                })))
            }
            tilo_actions::InvocationUpdate::Succeeded { output, .. } => {
                tracing::info!(%id, invocation = %invocation.0, ?output, "broker action completed");
                self.pace();
                Ok(Ok(StepOutcome { output }))
            }
            _ => Ok(Err(format!(
                "action {} was admitted but did not reach a terminal state in time",
                invocation.0
            ))),
        }
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
                Some(event @ CollabEvent::Document { .. }) => {
                    let _ = self.absorb_document_event(&event)?;
                }
                Some(event) => deferred.push(event),
                None => {}
            }
        }
    }

    /// Look for an instruction and carry it out.
    fn act(&mut self) -> Result<bool> {
        let Some(ed) = self.editor.as_ref() else {
            return Ok(false);
        };
        let blocks = plan::blocks(&ed.snapshot()?);
        // Who was addressed is decided here, from the document. What to do
        // about it is the only thing a model gets a say in.
        let Some((at, asked)) = plan::find_addressed(&blocks, &self.cfg.name) else {
            return Ok(false);
        };
        let Some(instruction_block) = blocks.get(at) else {
            return Ok(false);
        };
        let ins_id = instruction_block.id.clone();
        if self.handled.contains(&ins_id) {
            return Ok(false);
        }
        if !Self::settled(
            self.watching.get(&ins_id),
            &instruction_block.text,
            ui::time::now(),
            self.cfg.settle,
        ) {
            self.watching
                .insert(ins_id, (instruction_block.text.clone(), ui::time::now()));
            return Ok(false);
        }
        // Only an instruction about to be decided needs the catalogue: a
        // change that asks nothing of the agent never pays for its walk.
        self.refresh_stale_catalogue()?;
        self.watching.remove(&ins_id);
        self.handled.insert(ins_id.clone());

        let plan = self.decide(&ins_id, &asked, &blocks);
        let issued_by = self.instruction_issuer();
        let decision = self
            .last_route
            .as_ref()
            .map_or_else(|| "decision unavailable".to_string(), Route::detail);
        self.active_run = Some(ActiveRun {
            instruction: asked.clone(),
            issued_by,
            decision,
            group_id: format!("agent-run:{ins_id}"),
            steps: Vec::new(),
        });
        self.run_plan(&ins_id, &plan)
    }

    /// The latest non-agent author at the instant the instruction was read.
    /// This is intentionally evidence from the change graph, never a name
    /// parsed from the request. It is the best truthful issuer available for
    /// native document instructions, which do not themselves carry an author
    /// field.
    fn instruction_issuer(&mut self) -> Option<String> {
        let editor = self.editor.as_mut()?;
        let mine = editor.actor_hex();
        editor
            .versions()
            .into_iter()
            .rev()
            .find(|version| {
                !tilo_doc::model::is_schema_genesis(version)
                    && !version.actor.eq_ignore_ascii_case(&mine)
            })
            .map(|version| version.actor)
    }

    fn task_context(&self) -> Option<tilo_actions::ActionContext> {
        let run = self.active_run.as_ref()?;
        let record = self.cfg.record.as_deref().unwrap_or("active document");
        let mut context =
            tilo_actions::ActionContext::for_resource("Agent task", record, "document", record)
                .with_app(&self.cfg.app_id, "Agent")
                .with_purpose(&run.instruction)
                .with_group(&run.group_id, &run.instruction, "agent-run");
        if let Some(issuer) = &run.issued_by {
            context = context.with_facet(tilo_actions::ActionFacet::new("assigned-by", issuer));
        }
        Some(context)
    }

    fn capture_step(&mut self, app: &str, record: &str, summary: String, end: ui::HistoryEntryId) {
        let Some(run) = self.active_run.as_mut() else {
            return;
        };
        run.steps.push(ui::RunStep {
            app: app.to_string(),
            record: record.to_string(),
            run: ui::ChangeRun::single(end),
            summary,
        });
    }

    /// Commit the durable task record into the primary collaborative
    /// document. Run records are history markers, so the normal timeline
    /// remains focused on the work rather than bookkeeping.
    fn finish_run(&mut self) -> Result<()> {
        let Some(run) = self.active_run.take() else {
            return Ok(());
        };
        let Some(editor) = self.editor.as_mut() else {
            return Ok(());
        };
        editor.set_history_context(None);
        if run.steps.is_empty() {
            return Ok(());
        }
        if let Some(data) = editor.record_run(
            &run.instruction,
            run.issued_by.as_deref(),
            Some(&run.decision),
            &run.steps,
        ) {
            self.link.send(&CollabRequest::document_incremental(data))?;
        }
        for editor in self.peers.values_mut() {
            editor.set_history_context(None);
        }
        Ok(())
    }

    /// Whether a paragraph's text has stopped changing for long enough to
    /// be read as a finished request, rather than a line still being
    /// typed.
    ///
    /// A pure function of what was seen last time and what is seen now, so
    /// the debounce is checkable against an injected clock rather than a
    /// real wait. `watched` is `None` the first time this text
    /// is seen at all, which is never settled: there is nothing yet to
    /// compare it against.
    fn settled(
        watched: Option<&(String, Instant)>,
        text: &str,
        now: Instant,
        settle: Duration,
    ) -> bool {
        if settle.is_zero() {
            return true;
        }
        match watched {
            Some((seen, since)) if seen == text => now.saturating_duration_since(*since) >= settle,
            _ => false,
        }
    }

    /// Decide what `asked` asks for.
    ///
    /// A request written as one thing goes through the model exactly as
    /// before — the model answers one catalogue id for one instruction, and
    /// is not yet asked about a sequence. When that lands on nothing in the
    /// fixed catalogue, or the request is written as several things
    /// ([`plan::clauses`] finds an ordering word outside any quotation),
    /// the deterministic floor gets a turn against whatever the room's
    /// applications currently export — the model has no say in either of
    /// those, only in the single catalogue request that never needed it.
    fn decide(&mut self, ins_id: &str, asked: &str, blocks: &[Block]) -> Plan {
        let held = self.held();
        let catalogue = surface::Catalogue {
            offers: self.offers(&held),
            facts: surface::facts(&held),
            held: held
                .iter()
                .map(|(contract, _)| contract.namespace)
                .collect(),
        };
        let catalogue = match self
            .chat_run
            .as_ref()
            .and_then(|run| run.subject.as_deref())
        {
            Some(resource) => catalogue.scoped_to(resource),
            None => catalogue,
        };
        let offers = &catalogue.offers;
        let decision = self.chooser.choose_actions(asked, blocks, &catalogue, 8);
        let (plan, route) = match decision {
            Ok(answer) => {
                let mut steps = Vec::new();
                let mut invalid = None;
                for (id, arguments) in answer.actions.iter().zip(&answer.arguments) {
                    match Instruction::from_answer(id, asked, Some(arguments), offers) {
                        Ok(instruction) => steps.push(plan::Step::bound(instruction, offers)),
                        Err(error) => {
                            invalid = Some(format!("{error:?}"));
                            break;
                        }
                    }
                }
                if let Some(error) = invalid {
                    (
                        Plan::from_surface(asked, offers),
                        Route::Keywords(crate::model::Fallback::OutOfCatalogue(error)),
                    )
                } else {
                    if let Some(then) = answer.then {
                        steps.push(plan::Step::own(Instruction::Unknown(then)));
                    }
                    (Plan { steps }, Route::Model { why: answer.why })
                }
            }
            Err(route) => (Plan::from_surface(asked, offers), route),
        };
        tracing::info!(
            block = %ins_id,
            asked = %asked,
            steps = plan.len(),
            offers = offers.len(),
            decided_by = route.label(),
            live = route.is_live(),
            "{}",
            route.detail()
        );
        self.last_route = Some(route);
        plan
    }

    /// Carry out `plan`'s steps in order, against the instruction paragraph
    /// `ins_id`.
    ///
    /// A plan that cannot be finished is refused whole, before anything is
    /// touched — [`Plan::unanswered`] is the check, and the paragraph is
    /// left holding the refusal rather than a half-finished document.
    /// Otherwise each step reads a fresh snapshot, because the step before
    /// it may have changed the document; the instruction paragraph itself
    /// is removed once, at the end, so a step that needs to see it (`table`
    /// reads the bullets *above* it) can still do so wherever it falls in
    /// the sequence.
    fn run_plan(&mut self, ins_id: &str, plan: &Plan) -> Result<bool> {
        self.dispatched_this_turn.clear();
        let context = self.task_context();
        if let Some(editor) = self.editor.as_mut() {
            editor.set_history_context(context);
        }
        let blocks = plan::blocks(&self.editor.as_ref().context("no document")?.snapshot()?);
        if let Some(what) = plan.unanswered() {
            tracing::warn!(%what, steps = plan.len(), "refused a sequence whole; nothing was touched");
            self.splice_reply(
                &blocks,
                ins_id,
                &format!(
                    "{}: I can't \"{what}\", so I haven't done any of it.",
                    self.cfg.name
                ),
            )?;
            self.finish_run()?;
            return Ok(true);
        }
        let mut previous = None;
        for step in &plan.steps {
            let blocks = plan::blocks(&self.editor.as_ref().context("no document")?.snapshot()?);
            if !blocks.iter().any(|b| b.id == ins_id) {
                // A step earlier in this very plan already answered the
                // instruction and took the paragraph with it.
                break;
            }
            let mut outcome = None;
            let applied = match &step.instruction {
                // A surveyed operation, named by address. Guarded by
                // `dispatched_this_turn`: a clause asked again mid-plan can
                // come back naming the exact step this turn already performed.
                // Include bound arguments in the identity: assigning one role
                // to Ada and Lin, or creating several roles, is intentional
                // repeated use of one canonical action rather than a duplicate.
                Instruction::Perform {
                    id,
                    arguments,
                    confirmation,
                } => {
                    let arguments =
                        match arguments_for_step(step.carries, arguments, previous.as_ref()) {
                            Ok(arguments) => arguments,
                            Err(why) => {
                                self.splice_reply(
                                    &blocks,
                                    ins_id,
                                    &format!("{}: I couldn't do that - {why}", self.cfg.name),
                                )?;
                                self.finish_run()?;
                                return Ok(true);
                            }
                        };
                    let dispatch_key = dispatched_action_key(id, &arguments);
                    if self.dispatched_this_turn.contains(&dispatch_key) {
                        true
                    } else {
                        let id = id.clone();
                        match self.perform(&id, &arguments, confirmation)? {
                            Ok(performed) => {
                                self.dispatched_this_turn.insert(dispatch_key);
                                outcome = Some(performed);
                                true
                            }
                            Err(why) => {
                                let blocks = plan::blocks(
                                    &self.editor.as_ref().context("no document")?.snapshot()?,
                                );
                                self.splice_reply(
                                    &blocks,
                                    ins_id,
                                    &format!("{}: {why}", self.cfg.name),
                                )?;
                                false
                            }
                        }
                    }
                }
                Instruction::Unknown(what) => {
                    self.splice_reply(
                        &blocks,
                        ins_id,
                        &format!("{}: I can't \"{what}\".", self.cfg.name),
                    )?;
                    false
                }
            };
            if !applied {
                self.finish_run()?;
                return Ok(true);
            }
            // Only the immediately following step may consume a result. A
            // presentation-only step or a skipped duplicate breaks the chain
            // rather than letting a later action accidentally reuse stale
            // content.
            previous = outcome;
        }
        // Every step ran clean, so the instruction is answered in full and
        // the paragraph that asked for it goes away. Idempotent against a
        // step that already removed it (a plan of one, as before).
        if plan::blocks(&self.editor.as_ref().context("no document")?.snapshot()?)
            .iter()
            .any(|b| b.id == ins_id)
        {
            self.edit(ins_id, &wp::remove_blocks_action(vec![ins_id.to_string()]))?;
        }
        self.finish_run()?;
        Ok(true)
    }

    /// Replace the instruction paragraph's current text with `reply` — an
    /// answer left for somebody to read, not a further action.
    ///
    /// Local document mode only (a paragraph addressed to the agent by name).
    /// This and [`Self::edit`] are the last composed edits: they answer the
    /// instruction paragraph itself. They move to the paragraph's published
    /// operations (select the block, type or erase) once Writer publishes
    /// them; enrolled tasks never reach this.
    fn splice_reply(&mut self, blocks: &[Block], ins_id: &str, reply: &str) -> Result<()> {
        let n = blocks
            .iter()
            .find(|b| b.id == ins_id)
            .map_or(0, |b| b.text.chars().count());
        self.edit(
            ins_id,
            &wp::splice_text_action(RegionKind::Body, ins_id, 0, n, reply),
        )
    }

    /// Show the caret on `focus`, apply `action`, broadcast the change, and
    /// pause so the room can see it happen.
    #[allow(
        clippy::too_many_lines,
        reason = "one action lifecycle keeps invocation, attribution, broadcast, and user-visible reporting in a single auditable transaction"
    )]
    fn edit(&mut self, focus: &str, action: &RichTextAction) -> Result<()> {
        let before = self.editor.as_mut().and_then(Editor::latest_entry_id);
        let invocation = tilo_actions::InvocationId::new();
        let resource_id =
            <word_processor::framework::Document as ui::tilo_resource::Resource>::KIND.to_string();
        let agent_id = self.cfg.agent_id.clone();
        let app_id = self.cfg.app_id.clone();
        let resource_label = self
            .cfg
            .record
            .clone()
            .unwrap_or_else(|| "Open document".into());
        let group = self.task_context().and_then(|context| context.group);
        let semantic = |phase, sequence| tilo_actions::SemanticInteraction {
            version: tilo_actions::INTERACTION_VERSION,
            invocation,
            sequence,
            actor: tilo_actions::InteractionActor {
                id: agent_id.clone(),
                kind: tilo_actions::InteractionActorKind::Agent,
            },
            phase,
            action: tilo_actions::InteractionAction {
                id: "word-processor.edit".into(),
                label: describe(action),
                app: Some(tilo_actions::ActionApp {
                    id: app_id.clone(),
                    label: "Word Processor".into(),
                }),
            },
            resource: tilo_actions::ActionResource {
                id: resource_id.clone(),
                kind: resource_id.clone(),
                label: resource_label.clone(),
            },
            anchor: word_action_anchor(action, focus),
            group: group.clone(),
        };
        let interaction_key = PresenceKey::resource(&self.cfg.app_id, &resource_id, &resource_id);
        let _ = self.link.send(&CollabRequest::Presence {
            key: interaction_key.clone(),
            payload: PresencePayload::Interaction(Box::new(semantic(
                tilo_actions::InteractionPhase::Applying,
                0,
            ))),
        });
        let caret_pos = self
            .editor
            .as_ref()
            .and_then(|ed| ed.snapshot().ok())
            .and_then(|s| {
                s.blocks
                    .iter()
                    .find(|b| b.id == focus)
                    .map(|b| b.text.len())
            })
            .unwrap_or(0);
        self.link.send(&CollabRequest::Presence {
            key: block_key(focus),
            payload: PresencePayload::Caret { pos: caret_pos },
        })?;
        let editor = self.editor.as_mut().context("no document yet")?;
        let delta =
            editor.apply::<word_processor::framework::Document>(std::slice::from_ref(action))?;
        if let Some(data) = delta {
            self.link.send(&CollabRequest::document_incremental(data))?;
            if let RichTextAction::SpliceText { at, ins, .. } = action
                && !ins.is_empty()
            {
                let end = at.saturating_add(ins.chars().count());
                self.link.send(&CollabRequest::Presence {
                    key: block_key(focus),
                    payload: PresencePayload::Typing { start: *at, end },
                })?;
                self.link.send(&CollabRequest::Presence {
                    key: block_key(focus),
                    payload: PresencePayload::Caret { pos: end },
                })?;
            }
            let _ = self.link.send(&CollabRequest::Presence {
                key: interaction_key,
                payload: PresencePayload::Interaction(Box::new(semantic(
                    tilo_actions::InteractionPhase::Committed,
                    1,
                ))),
            });
            tracing::info!(block = %focus, "applied {}", describe(action));
            if let Some(end) = self.editor.as_mut().and_then(Editor::latest_entry_id)
                && Some(end) != before
            {
                let record = self.cfg.record.as_deref().unwrap_or_default().to_string();
                let app = self.cfg.app_id.clone();
                self.capture_step(&app, &record, describe(action), end);
            }
        } else {
            let _ = self.link.send(&CollabRequest::Presence {
                key: interaction_key,
                payload: PresencePayload::Interaction(Box::new(semantic(
                    tilo_actions::InteractionPhase::Failed {
                        code: Some("no_change".into()),
                        message: "The operation made no change".into(),
                    },
                    1,
                ))),
            });
            tracing::warn!(block = %focus, "{} changed nothing", describe(action));
        }
        self.pace();
        Ok(())
    }

    /// The demo pause after a visible action ([`Config::pace`]); none by
    /// default.
    fn pace(&self) {
        if !self.cfg.pace.is_zero() {
            std::thread::sleep(self.cfg.pace);
        }
    }
}

fn comment_text_anchor(block: &str, start: usize, end: usize) -> tilo_actions::InteractionAnchor {
    tilo_actions::InteractionAnchor::Text {
        region: "document".into(),
        ranges: vec![tilo_actions::InteractionTextRange {
            block: block.to_string(),
            start: u64::try_from(start).unwrap_or(u64::MAX),
            end: u64::try_from(end).unwrap_or(u64::MAX),
        }],
    }
}

fn selection_text_anchor(
    start_block: &str,
    start: usize,
    end_block: &str,
    end: usize,
) -> tilo_actions::InteractionAnchor {
    let range = |block: &str, start, end| tilo_actions::InteractionTextRange {
        block: block.to_string(),
        start: u64::try_from(start).unwrap_or(u64::MAX),
        end: u64::try_from(end).unwrap_or(u64::MAX),
    };
    tilo_actions::InteractionAnchor::Text {
        region: "document".into(),
        ranges: if start_block == end_block {
            vec![range(start_block, start, end)]
        } else {
            vec![range(start_block, start, start), range(end_block, end, end)]
        },
    }
}

fn object_anchor(id: &str) -> tilo_actions::InteractionAnchor {
    tilo_actions::InteractionAnchor::Objects {
        ids: vec![id.to_string()],
    }
}

fn word_action_anchor(action: &RichTextAction, _focus: &str) -> tilo_actions::InteractionAnchor {
    match action {
        RichTextAction::SpliceText {
            block,
            at,
            del,
            ins,
            ..
        } => tilo_actions::InteractionAnchor::Text {
            region: "document".into(),
            ranges: vec![tilo_actions::InteractionTextRange {
                block: block.clone(),
                start: u64::try_from(*at).unwrap_or(u64::MAX),
                end: u64::try_from(at.saturating_add((*del).max(ins.chars().count())))
                    .unwrap_or(u64::MAX),
            }],
        },
        RichTextAction::ReplaceSelection {
            start_block,
            start,
            end_block,
            end,
            ..
        }
        | RichTextAction::PasteFragment {
            start_block,
            start,
            end_block,
            end,
            ..
        } => selection_text_anchor(start_block, *start, end_block, *end),
        RichTextAction::SetMark { spans, .. } | RichTextAction::ReplaceMatches { spans, .. } => {
            tilo_actions::InteractionAnchor::Text {
                region: "document".into(),
                ranges: spans
                    .iter()
                    .map(|span| tilo_actions::InteractionTextRange {
                        block: span.block.clone(),
                        start: u64::try_from(span.start).unwrap_or(u64::MAX),
                        end: u64::try_from(span.end).unwrap_or(u64::MAX),
                    })
                    .collect(),
            }
        }
        RichTextAction::SetParagraphStyle { blocks, .. }
        | RichTextAction::ApplyNamedStyle { blocks, .. }
        | RichTextAction::SetBlockAttr { blocks, .. }
        | RichTextAction::ClearBlockAttr { blocks, .. }
        | RichTextAction::RemoveBlocks { blocks }
        | RichTextAction::RemoveRegionBlocks { blocks, .. }
        | RichTextAction::MoveBlocks { blocks, .. } => tilo_actions::InteractionAnchor::Objects {
            ids: blocks.clone(),
        },
        RichTextAction::SetPageMargins { .. }
        | RichTextAction::GenerateToc { .. }
        | RichTextAction::ReplaceBody { .. }
        | RichTextAction::ReplaceFormatted { .. }
        | RichTextAction::NoteFileWritten { .. }
        | RichTextAction::EnsureRegionBlock { .. } => tilo_actions::InteractionAnchor::Resource,
        RichTextAction::SetStyleProperty { style, .. }
        | RichTextAction::DeleteStyle { style, .. } => object_anchor(style),
        RichTextAction::EditTable { table, .. } => object_anchor(table),
        RichTextAction::InsertParagraph { after, .. }
        | RichTextAction::InsertTable { after, .. }
        | RichTextAction::InsertFilledTable { after, .. } => object_anchor(after),
        RichTextAction::InsertImage { block, .. }
        | RichTextAction::InsertPageBreak { block, .. }
        | RichTextAction::SetImageCrop { block, .. }
        | RichTextAction::SetImageSize { block, .. }
        | RichTextAction::SetImagePlacement { block, .. } => object_anchor(block),
        RichTextAction::SplitBlock { block, at, .. }
        | RichTextAction::InsertField { block, at, .. } => comment_text_anchor(block, *at, *at),
        RichTextAction::AcceptChanges { id } | RichTextAction::RejectChanges { id } => id
            .as_ref()
            .map_or(tilo_actions::InteractionAnchor::Resource, |id| {
                tilo_actions::InteractionAnchor::Objects {
                    ids: vec![id.clone()],
                }
            }),
        RichTextAction::ResolveComment { id } => tilo_actions::InteractionAnchor::Objects {
            ids: vec![id.clone()],
        },
        RichTextAction::TrackInsert {
            block, at, text, ..
        } => comment_text_anchor(block, *at, at.saturating_add(text.chars().count())),
        RichTextAction::AddComment {
            block, start, end, ..
        } => comment_text_anchor(block, *start, *end),
        RichTextAction::ReplyToComment { comment, .. } => {
            tilo_actions::InteractionAnchor::Objects {
                ids: vec![comment.clone()],
            }
        }
        // Resolved against the end of the body at apply time.
        RichTextAction::AppendParagraph { .. } | RichTextAction::ContinueParagraph { .. } => {
            tilo_actions::InteractionAnchor::Resource
        }
        RichTextAction::InsertBlocks { at, .. } | RichTextAction::InsertMarkdown { at, .. } => {
            match at {
                tilo_document::block_edit::Placement::After(id) => object_anchor(id),
                tilo_document::block_edit::Placement::Start
                | tilo_document::block_edit::Placement::End => {
                    tilo_actions::InteractionAnchor::Resource
                }
            }
        }
        RichTextAction::SetBlockText { block, .. }
        | RichTextAction::SetBlockShape { block, .. }
        | RichTextAction::SetBlockDone { block, .. }
        | RichTextAction::ShiftIndent { block, .. }
        | RichTextAction::MoveBlock { block, .. } => object_anchor(block),
        RichTextAction::DeleteBlocks { blocks } => tilo_actions::InteractionAnchor::Objects {
            ids: blocks.clone(),
        },
        RichTextAction::SyncBody { .. } => tilo_actions::InteractionAnchor::Resource,
    }
}

fn describe(action: &RichTextAction) -> String {
    match action {
        RichTextAction::SpliceText { ins, del, .. } => {
            format!("splice text (+{} -{del})", ins.chars().count())
        }
        RichTextAction::InsertTable { rows, cols, .. } => {
            format!("insert table {rows}x{cols}")
        }
        RichTextAction::InsertParagraph { .. } => "insert paragraph".to_string(),
        RichTextAction::RemoveBlocks { blocks } => format!("remove {} block(s)", blocks.len()),
        RichTextAction::MoveBlocks { blocks, .. } => format!("move {} block(s)", blocks.len()),
        RichTextAction::SetParagraphStyle { kind, .. } => format!("set style {kind}"),
        other => format!("{other:?}"),
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
#[path = "../tests/typed_peer.rs"]
mod handoff_test_peer;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_matches_anchors_every_original_character_range() {
        let action = RichTextAction::ReplaceMatches {
            region: word_processor::actions::RegionKind::Body,
            spans: vec![
                word_processor::actions::Span {
                    block: "first".into(),
                    start: 3,
                    end: 8,
                },
                word_processor::actions::Span {
                    block: "second".into(),
                    start: 7,
                    end: 12,
                },
            ],
            ins: "β😀".into(),
        };
        assert_eq!(
            word_action_anchor(&action, "unrelated-focus"),
            tilo_actions::InteractionAnchor::Text {
                region: "document".into(),
                ranges: vec![
                    tilo_actions::InteractionTextRange {
                        block: "first".into(),
                        start: 3,
                        end: 8
                    },
                    tilo_actions::InteractionTextRange {
                        block: "second".into(),
                        start: 7,
                        end: 12
                    },
                ],
            }
        );
    }

    #[test]
    fn word_actions_anchor_the_exact_document_objects_they_change() {
        // Case `image_layout_actions_anchor_the_exact_document_object`.
        {
            let actions = [
                RichTextAction::SetImageCrop {
                    block: "image-7".into(),
                    left: 250,
                    top: 250,
                    right: 250,
                    bottom: 250,
                },
                RichTextAction::SetImageSize {
                    block: "image-7".into(),
                    width: 300,
                    height: 180,
                },
                RichTextAction::SetImagePlacement {
                    block: "image-7".into(),
                    anchor: "page".into(),
                    wrap: "none".into(),
                    x: 24,
                    y: 36,
                    page: 0,
                },
            ];

            for action in actions {
                assert_eq!(
                    word_action_anchor(&action, "unused-focus"),
                    tilo_actions::InteractionAnchor::Objects {
                        ids: vec!["image-7".into()],
                    }
                );
            }
        }

        // Case `comment_actions_anchor_their_shared_document_objects`.
        {
            let add = RichTextAction::AddComment {
                block: "paragraph-7".into(),
                start: 4,
                end: 11,
                author: "Scribe".into(),
                date: "2026-09-08".into(),
                text: "Please clarify.".into(),
            };
            assert_eq!(
                word_action_anchor(&add, "unused-focus"),
                tilo_actions::InteractionAnchor::Text {
                    region: "document".into(),
                    ranges: vec![tilo_actions::InteractionTextRange {
                        block: "paragraph-7".into(),
                        start: 4,
                        end: 11,
                    }],
                }
            );

            let reply = RichTextAction::ReplyToComment {
                comment: "comment-3".into(),
                author: "Scribe".into(),
                date: "2026-09-08".into(),
                text: "Done.".into(),
                body: Vec::new(),
            };
            assert_eq!(
                word_action_anchor(&reply, "unused-focus"),
                tilo_actions::InteractionAnchor::Objects {
                    ids: vec!["comment-3".into()],
                }
            );
        }
    }

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

    mod plan_actions {
        use super::*;
        use std::{cell::RefCell, rc::Rc};
        use ui::collaboration::{BrokerActionDescriptor, BrokerActionInput, BrokerActionSurface};

        /// Admits every invocation as still running and reports it succeeded
        /// once it is observed.
        struct Host {
            events: Vec<CollabEvent>,
            observed: Rc<RefCell<usize>>,
        }

        impl ui::collaboration::PeerTransport for Host {
            fn is_connected(&self) -> bool {
                true
            }
            fn poll(&mut self) -> Vec<CollabEvent> {
                std::mem::take(&mut self.events)
            }
            fn send(&mut self, request: CollabRequest) -> Result<(), String> {
                let message = match request {
                    CollabRequest::RemoteAction {
                        message: RemoteActionMessage::Invoke(invoke),
                    } => RemoteActionMessage::Receipt(tilo_actions::remote::InvocationReceipt {
                        invocation: invoke.invocation,
                        status: tilo_actions::remote::ReceiptStatus::Accepted,
                    }),
                    CollabRequest::RemoteAction {
                        message: RemoteActionMessage::Observe { invocation, .. },
                    } => {
                        *self.observed.borrow_mut() += 1;
                        RemoteActionMessage::Update {
                            invocation,
                            state: Some(tilo_actions::remote::InvocationState::new(
                                invocation,
                                tilo_actions::InvocationUpdate::Succeeded {
                                    invocation,
                                    output: tilo_actions::ActionOutput::default(),
                                },
                            )),
                        }
                    }
                    CollabRequest::Presence { .. } => return Ok(()),
                    other => panic!("unexpected request {other:?}"),
                };
                self.events.push(CollabEvent::RemoteAction { message });
                Ok(())
            }
        }

        /// One live document operation, as Writer publishes it.
        fn surfaces() -> Vec<BrokerActionSurface> {
            vec![BrokerActionSurface {
                peer_id: 7,
                app_id: "so.tilo.word-processor".into(),
                cluster_id: Uuid::from_u128(1).to_string(),
                record_id: String::new(),
                live: true,
                actions: vec![BrokerActionDescriptor {
                    semantic_identity: None,
                    resource: "word-processor.document".into(),
                    action: "insert-heading".into(),
                    resource_kind: "word_processor.document".into(),
                    resource_label: "Brief".into(),
                    action_label: "Insert a heading".into(),
                    description: None,
                    operation_kind: tilo_actions::OperationKind::Document,
                    params: Vec::new(),
                    contract: tilo_actions::ActionContract::default(),
                    input: BrokerActionInput {
                        kind: "none".into(),
                        prompt: String::new(),
                        placeholder: String::new(),
                        wants_content: false,
                    },
                }],
            }]
        }

        /// A document-mode session with the pace every launcher starts with.
        fn session(link: Link, broker_surfaces: Vec<BrokerActionSurface>) -> Session {
            Session {
                cfg: Config {
                    agent_id: "agent-1".into(),
                    endpoint: None,
                    endpoint_identity: None,
                    app_id: "so.tilo.word-processor".into(),
                    cluster: Uuid::from_u128(1),
                    record: None,
                    name: "agent-1".into(),
                    pace: Duration::from_millis(DEFAULT_PACE_MS),
                    once: false,
                    model: false,
                    model_timeout: Duration::ZERO,
                    await_document: Duration::ZERO,
                    settle: Duration::ZERO,
                    permissions: PermissionContext::from_capabilities(
                        ui::permissions::Actor::agent("agent-1"),
                        vec![],
                    ),
                    content: tilo_content::ContentStore::memory(),
                },
                link,
                editor: None,
                peers: BTreeMap::new(),
                handled: HashSet::new(),
                watching: HashMap::new(),
                dispatched_this_turn: HashSet::new(),
                chooser: Chooser::new(false, Duration::ZERO),
                enrolled_custody: None,
                enrolled_active: None,
                enrolled_sequence: 0,
                task_deadline: None,
                asked_at: ui::time::now(),
                last_route: None,
                broker_catalogue: tilo_actions::remote::CataloguePage::default(),
                catalogue_fetched: None,
                broker_surfaces,
                next_broker_request: 1,
                active_run: None,
                chat_run: None,
                chat_pending: false,
                chat_catalogue_received: false,
                last_chat_action: None,
                chat_sources: HashMap::new(),
                live: crate::live::LivePublisher::default(),
                memory: agent_loop::Memory::default(),
                broker_revision: 0,
                catalogue_cache: None,
                turn: None,
                outstanding: outstanding::Outstanding::default(),
                native_task: None,
            }
        }

        /// A plan's brokered action is admitted and still running, so the
        /// agent follows it to its result — at once, not after a fixed sleep
        /// — and with the default pace it does not pause afterwards either.
        /// Eight actions used to take eight 50 ms sleeps before observing and,
        /// at the old default pace, eight 250 ms pauses after.
        #[test]
        fn plan_actions_follow_their_result_without_pausing() {
            const ACTIONS: usize = 8;
            let observed = Rc::new(RefCell::new(0));
            let link = crate::wire::test_link(Host {
                events: Vec::new(),
                observed: observed.clone(),
            });
            let surfaces = surfaces();
            let offer = surface::broker_offers(&surfaces).remove(0);
            let mut session = session(link, surfaces);
            let began = ui::time::now();
            for _ in 0..ACTIONS {
                let outcome = session
                    .perform_broker(
                        &offer.id(),
                        &offer,
                        &serde_json::Map::new(),
                        &ConfirmationEvidence::None,
                    )
                    .unwrap();
                assert!(outcome.is_ok(), "{outcome:?}");
            }
            let took = began.elapsed();
            assert_eq!(
                *observed.borrow(),
                ACTIONS,
                "each action is followed to its result"
            );
            assert!(
                took < Duration::from_millis(250),
                "{ACTIONS} plan actions must not sleep or pause: {took:?}"
            );
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
                        permissions: PermissionContext::from_capabilities(
                            ui::permissions::Actor::agent("agent-1"),
                            vec![],
                        ),
                        content: tilo_content::ContentStore::memory(),
                    },
                    link,
                    editor: None,
                    peers: BTreeMap::new(),
                    handled: HashSet::new(),
                    watching: HashMap::new(),
                    dispatched_this_turn: HashSet::new(),
                    chooser: Chooser::new(false, Duration::ZERO),
                    enrolled_custody: managed.then_some(custody),
                    enrolled_active: managed.then_some(acceptance),
                    enrolled_sequence: 0,
                    task_deadline: None,
                    native_task: None,
                    asked_at: ui::time::now(),
                    last_route: None,
                    broker_catalogue: tilo_actions::remote::CataloguePage::default(),
                    catalogue_fetched: None,
                    broker_surfaces: Vec::new(),
                    next_broker_request: 1,
                    active_run: None,
                    chat_run: None,
                    chat_pending: false,
                    chat_catalogue_received: false,
                    last_chat_action: None,
                    chat_sources: HashMap::new(),
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
                    carries: false,
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
                    permissions: PermissionContext::from_capabilities(
                        ui::permissions::Actor::agent("agent-1"),
                        vec![],
                    ),
                    content: tilo_content::ContentStore::memory(),
                },
                link,
                editor: None,
                peers: BTreeMap::new(),
                handled: HashSet::new(),
                watching: HashMap::new(),
                dispatched_this_turn: HashSet::new(),
                chooser: Chooser::new(false, Duration::ZERO),
                enrolled_custody: Some(custody),
                enrolled_active: Some(acceptance),
                enrolled_sequence: 0,
                task_deadline: None,
                native_task: None,
                asked_at: ui::time::now(),
                last_route: None,
                broker_catalogue: tilo_actions::remote::CataloguePage::default(),
                catalogue_fetched: None,
                broker_surfaces: Vec::new(),
                next_broker_request: 1,
                active_run: None,
                chat_run: None,
                chat_pending: false,
                chat_catalogue_received: false,
                last_chat_action: None,
                chat_sources: HashMap::new(),
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
                carries,
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
                second.carries,
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

            let bound = arguments_for_step(true, &planned, Some(&previous))
                .expect("one content result binds the next content input");
            assert_eq!(
                bound.get(tilo_actions::PROMPTED_PARAM),
                Some(&serde_json::Value::String("abc123".into())),
                "the preceding app result must replace model input"
            );
            assert!(
                arguments_for_step(true, &planned, None).is_err(),
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
                arguments_for_step(true, &serde_json::Map::new(), Some(&ambiguous)).is_err(),
                "choosing one of two products would be a model-visible guess"
            );

            let wrong_kind = StepOutcome {
                output: tilo_actions::ActionOutput {
                    value: None,
                    resources: vec!["files.entry:/tmp/picture.png".into()],
                },
            };
            assert!(
                arguments_for_step(true, &serde_json::Map::new(), Some(&wrong_kind)).is_err(),
                "a generic resource address is not proof that content bytes exist"
            );
        }

        // Case `an_empty_result_feeds_no_content_step`.
        {
            let previous = StepOutcome::default();
            assert!(
                arguments_for_step(true, &serde_json::Map::new(), Some(&previous)).is_err(),
                "a step that produced nothing cannot feed a content input"
            );
        }

        // Case `repeated_action_ids_remain_distinct_when_arguments_differ`.
        {
            let ada = serde_json::Map::from_iter([(
                "subject".into(),
                serde_json::Value::String("ada".into()),
            )]);
            let lin = serde_json::Map::from_iter([(
                "subject".into(),
                serde_json::Value::String("lin".into()),
            )]);
            assert_eq!(
                dispatched_action_key("profile.assign", &ada),
                dispatched_action_key("profile.assign", &ada)
            );
            assert_ne!(
                dispatched_action_key("profile.assign", &ada),
                dispatched_action_key("profile.assign", &lin)
            );
        }
    }

    #[test]
    fn a_line_settles_only_once_unchanged_for_the_whole_wait() {
        // Case `a_zero_settle_never_waits`.
        {
            let now = ui::time::now();
            assert!(Session::settled(None, "Scribe: table", now, Duration::ZERO));
            assert!(Session::settled(
                Some(&("Scribe: table".to_string(), now)),
                "Scribe: table",
                now,
                Duration::ZERO
            ));
        }

        // Case `the_first_sight_of_a_line_is_never_settled`.
        {
            let now = ui::time::now();
            assert!(!Session::settled(
                None,
                "Scribe: table",
                now,
                Duration::from_millis(500)
            ));
        }

        // Case `a_line_that_changed_since_it_was_last_seen_is_not_settled`.
        {
            let now = ui::time::now();
            let watched = ("Scribe: tab".to_string(), now);
            assert!(!Session::settled(
                Some(&watched),
                "Scribe: table",
                now,
                Duration::from_millis(500)
            ));
        }

        // Case `unchanged_text_settles_once_the_wait_has_elapsed_and_not_before`.
        {
            let settle = Duration::from_millis(500);
            let seen = ui::time::now();
            let watched = ("Scribe: table".to_string(), seen);
            assert!(
                !Session::settled(
                    Some(&watched),
                    "Scribe: table",
                    seen + Duration::from_millis(499),
                    settle
                ),
                "one millisecond short of the wait must not settle"
            );
            assert!(Session::settled(
                Some(&watched),
                "Scribe: table",
                seen + Duration::from_millis(500),
                settle
            ));
            assert!(Session::settled(
                Some(&watched),
                "Scribe: table",
                seen + Duration::from_secs(5),
                settle
            ));
        }
    }

    mod handoff_sequences {
        use super::*;
        use crate::session::handoff_test_peer as peer;
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        };
        use tilo_actions::{ActionOutput, ResultSpec};
        use tilo_content::ContentStore;

        const PHOTOS: &str = "so.tilo.photos";
        const WRITER: &str = "so.tilo.word-processor";
        const PRODUCE: &str = "export-edited-content";
        const RECORD: &str = "file:/handoff-sequence.docx";
        static SERIAL: Mutex<()> = Mutex::new(());
        type Calls = Vec<(String, serde_json::Map<String, serde_json::Value>)>;

        #[derive(Clone, Copy)]
        enum Between {
            Nothing,
            BrokerView,
            LocalRotate,
            Duplicate,
        }

        struct Observation {
            calls: Calls,
            instruction: String,
            images: Vec<String>,
            rotation: u8,
        }

        /// A Photos export's output, its bytes put in the `content` store
        /// both peers of the fixture share.
        fn produced(content: &ContentStore) -> ActionOutput {
            let bytes = photos::pixels::encode_png(&photos::shots_pattern(5, 3)).unwrap();
            let hash = content.put(&bytes).unwrap();
            ActionOutput {
                value: None,
                resources: vec![
                    tilo_actions::content_resource(&hash)
                        .expect("the producer returned a valid content hash"),
                ],
            }
        }

        fn content_result() -> ResultSpec {
            ResultSpec::Resource {
                resource_kind: tilo_actions::CONTENT_RESOURCE_KIND.into(),
            }
        }

        fn writer_resources(
            doc: &automerge::AutoCommit,
            caret: &str,
            content: &ContentStore,
        ) -> Vec<tilo_actions::Resource> {
            wp::published_for(
                &wp::WpResourceCtx {
                    caret_block: caret.into(),
                    caret_offset: 0,
                    target_blocks: vec![caret.into()],
                    format_spans: Vec::new(),
                    selection: None,
                    region: RegionKind::Body,
                    search_hits: Vec::new(),
                    content: content.clone(),
                },
                doc,
            )
        }

        fn connect_peer(
            path: &std::path::Path,
            app: &str,
            handle: &str,
            record: Option<&str>,
        ) -> peer::TypedPeer {
            let mut client = peer::TypedPeer::connect(path);
            peer::send(
                &mut client,
                &CollabRequest::Hello {
                    app_id: app.into(),
                    cluster_id: peer::room(),
                    record: record.map(str::to_owned),
                    record_kind: None,
                    handle: Some(handle.into()),
                },
            )
            .unwrap();
            client.set_read_timeout(Some(Duration::from_millis(20)));
            client
        }

        // These are controlled application outcomes, carried by the real broker.
        // The None/wrong-kind cases declare matching contracts: the broker must
        // accept them, leaving the real agent loop to refuse a content handoff.
        // Photos pixel production is covered separately by the native proof.
        #[allow(
            clippy::too_many_lines,
            reason = "one fixture owns both app peers and the real Session plan lifecycle"
        )]
        fn run_sequence(
            content: &ContentStore,
            output: &ActionOutput,
            result: &ResultSpec,
            between: Between,
            failed: bool,
        ) -> Observation {
            let _serial = SERIAL
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("collab.sock");
            let mut photo = automerge::AutoCommit::new();
            let source = photos::pixels::encode_png(&photos::shots_pattern(3, 2)).unwrap();
            let source_hash = content.put(&source).unwrap();
            let held = photos::model::LibraryPhoto::from_image(
                photos::OpenImage::from_path("held.png", 3, 2).with_content(source_hash),
            );
            let image_id = held.id.clone();
            tilo_actions::Action::apply(
                &photos::actions::PhotosAction::ImportPhoto { photo: held },
                &mut photo,
            )
            .unwrap();
            // Exactly the open photograph's production and view, and the
            // document's placement: grants name the resource they are on.
            let _broker = peer::spawn_with_agent_actions(
                &socket,
                &[
                    (PHOTOS, image_id.as_str(), PRODUCE),
                    (PHOTOS, image_id.as_str(), "view"),
                    (WRITER, peer::WP_DOCUMENT, "place-image"),
                ],
            );
            let mut document = automerge::AutoCommit::new();
            let model = tilo_doc::model::init(&mut document, &automerge::ROOT);
            let mut ids = tilo_doc::model::seeded_id_gen(&document, &model);
            let instruction_id = {
                let mut edit = tilo_doc::DocMut {
                    doc: &mut document,
                    c: &model,
                    ids: &mut ids,
                };
                let id = edit.insert_block(None, "paragraph");
                edit.splice_text(&id, 0, 0, "Scribe: run the reviewed handoff plan");
                id
            };
            document.commit();
            let calls = Arc::new(Mutex::new(Calls::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let mut threads = Vec::new();
            for is_photo in [true, false] {
                let worker_socket = socket.clone();
                let photo = photo.clone();
                let result = result.clone();
                let calls = Arc::clone(&calls);
                let stop = Arc::clone(&stop);
                let output = output.clone();
                let content = content.clone();
                let mut replica = document.clone();
                let caret = instruction_id.clone();
                let open_image = image_id.clone();
                threads.push(std::thread::spawn(move || {
                    // Each typed manager is constructed and used on its owner thread.
                    let mut client = if is_photo {
                        let mut client = connect_peer(&worker_socket, PHOTOS, "photos", None);
                        // The open photograph, as Photos publishes it.
                        let mut photo_resources: Vec<_> =
                            surface::tree(surface::for_app_id(PHOTOS).unwrap(), &photo)
                                .into_iter()
                                .flat_map(|library| library.children)
                                .filter(|child| child.id.as_str() == open_image)
                                .collect();
                        for resource in &mut photo_resources {
                            resource.children.clear();
                            resource
                                .actions
                                .retain(|action| action.id == PRODUCE || action.id == "view");
                            for action in &mut resource.actions {
                                if action.id == PRODUCE {
                                    action.contract.result = result.clone();
                                }
                            }
                        }
                        peer::publish_actions(&mut client, PHOTOS, &photo_resources);
                        client
                    } else {
                        let mut client =
                            connect_peer(&worker_socket, WRITER, "ada", Some(RECORD));
                        peer::publish_actions(
                            &mut client,
                            WRITER,
                            &writer_resources(&replica, &caret, &content),
                        );
                        client
                    };
                    while !stop.load(Ordering::Acquire) {
                        let Some(event) = client.recv().unwrap() else {
                            continue;
                        };
                        if let CollabEvent::Invocation {
                            id,
                            invocation,
                            resource,
                            action,
                            argument,
                            actor,
                            context,
                            ..
                        } = event
                        {
                            let json = argument
                                .as_deref()
                                .unwrap()
                                .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                                .expect("Session must dispatch the typed JSON argument envelope");
                            let arguments = serde_json::from_str(json).unwrap();
                            calls.lock().unwrap().push((action.clone(), arguments));
                            let update = if is_photo {
                                if failed && action == PRODUCE {
                                    tilo_actions::InvocationUpdate::Failed {
                                        invocation,
                                        error: tilo_actions::ActionFailure {
                                            code: "fixture.failed".into(),
                                            message: "Producer refused".into(),
                                            details: None,
                                            retryable: false,
                                        },
                                    }
                                } else {
                                    tilo_actions::InvocationUpdate::Succeeded {
                                        invocation,
                                        output: if action == PRODUCE {
                                            output.clone()
                                        } else {
                                            ActionOutput::default()
                                        },
                                    }
                                }
                            } else {
                                let resources = writer_resources(&replica, &caret, &content);
                                peer::service_document_invocation(
                                    &mut client,
                                    &mut replica,
                                    &resources,
                                    wp::register,
                                    peer::DocumentInvocation {
                                        invocation,
                                        address: tilo_actions::ActionAddress { resource, action },
                                        argument: argument.as_deref(),
                                        actor: &actor,
                                        context: context.as_deref(),
                                    },
                                )
                            };
                            client
                                .send(&CollabRequest::UpdateInvocation { id, update })
                                .unwrap();
                        }
                    }
                }));
            }
            let permissions = PermissionContext::from_capabilities(
                ui::permissions::Actor::agent("Scribe"),
                [PHOTOS, WRITER]
                    .into_iter()
                    .map(|app| {
                        ui::permissions::CapOverride::grant(
                            ui::permissions::Subject::agent("Scribe"),
                            ui::permissions::Scope::App(app.into()),
                            ui::permissions::Verb::Write,
                        )
                    })
                    .collect(),
            );
            let mut session = Session::connect(Config {
                endpoint: Some(socket),
                endpoint_identity: Some(peer::endpoint_identity("Scribe")),
                app_id: WRITER.into(),
                cluster: peer::room(),
                record: Some(RECORD.into()),
                agent_id: "Scribe".into(),
                name: "Scribe".into(),
                pace: Duration::ZERO,
                once: true,
                model: false,
                model_timeout: Duration::from_millis(1),
                await_document: Duration::from_secs(5),
                settle: Duration::ZERO,
                permissions,
                content: content.clone(),
            })
            .unwrap();
            session.editor = Some(
                Editor::from_snapshot(
                    surface::for_app_id(WRITER).unwrap(),
                    &document.save(),
                    "Scribe",
                    session.cfg.permissions.clone(),
                )
                .unwrap(),
            );
            // A usable held photo is essential: it exposes the forbidden
            // broker-result fallback that empty-replica tests would miss.
            session.peers.insert(
                "photos",
                Editor::peer_from_snapshot_on_route(
                    surface::for_app_id(PHOTOS).unwrap(),
                    PHOTOS,
                    &photo.save(),
                    "Scribe",
                    session.cfg.permissions.clone(),
                )
                .unwrap(),
            );
            let deadline = tilo_test_env::hang_deadline();
            let offers = loop {
                assert!(
                    ui::time::now() < deadline,
                    "both app catalogues must arrive"
                );
                session.refresh_catalogue().unwrap();
                let offers = Session::document_broker_offers(
                    &session.broker_catalogue,
                    &session.cfg.permissions,
                );
                if [PRODUCE, "view", "place-image"]
                    .iter()
                    .all(|id| offers.iter().any(|offer| offer.address().action == *id))
                {
                    break offers;
                }
            };
            let bound_step = |action: &str, arguments| {
                let offer = offers
                    .iter()
                    .find(|offer| offer.address().action == action)
                    .unwrap();
                plan::Step::bound(
                    Instruction::Perform {
                        id: offer.id(),
                        arguments,
                        confirmation: ConfirmationEvidence::None,
                    },
                    &offers,
                )
            };
            let producer = bound_step(PRODUCE, serde_json::Map::new());
            let mut steps = vec![producer.clone()];
            match between {
                Between::Nothing => {}
                Between::BrokerView => steps.push(bound_step("view", serde_json::Map::new())),
                Between::Duplicate => steps.push(producer),
                Between::LocalRotate => steps.push(plan::Step::own(Instruction::Perform {
                    id: surface::encode_address(&tilo_actions::ActionAddress::new(
                        image_id,
                        "rotate-right",
                    )),
                    arguments: serde_json::Map::new(),
                    confirmation: ConfirmationEvidence::None,
                })),
            }
            steps.push(bound_step(
                "place-image",
                serde_json::Map::from_iter([(
                    tilo_actions::PROMPTED_PARAM.into(),
                    serde_json::Value::String("model-invented-hash".into()),
                )]),
            ));
            let outcome = session.run_plan(&instruction_id, &Plan { steps });
            stop.store(true, Ordering::Release);
            for thread in threads {
                thread.join().unwrap();
            }
            assert!(
                outcome.unwrap(),
                "the actual plan loop must complete or report its refusal"
            );
            let snapshot = session.editor.as_ref().unwrap().snapshot().unwrap();
            Observation {
                calls: calls.lock().unwrap().clone(),
                instruction: snapshot
                    .blocks
                    .iter()
                    .find(|block| block.id == instruction_id)
                    .map(|block| block.text.clone())
                    .unwrap_or_default(),
                images: snapshot
                    .blocks
                    .iter()
                    .filter(|block| block.kind == "image")
                    .filter_map(|block| match block.attrs.get("blob") {
                        Some(tilo_doc::AttrVal::Str(hash)) => Some(hash.clone()),
                        _ => None,
                    })
                    .collect(),
                rotation: photos::doc::PhotosDoc::library(session.peers["photos"].doc())
                    .photos
                    .first()
                    .map_or(0, |photo| photo.edits.rotate_quarters),
            }
        }

        fn assert_no_consumer(observed: &Observation, expected_calls: &[&str]) {
            assert_eq!(
                observed
                    .calls
                    .iter()
                    .map(|(action, _)| action.as_str())
                    .collect::<Vec<_>>(),
                expected_calls,
                "an invalidated or missing product must stop before dispatching Writer"
            );
            assert!(
                observed.images.is_empty(),
                "the refusal must not insert an image"
            );
            assert!(
                !observed.instruction.is_empty(),
                "the instruction must retain the refusal"
            );
        }

        /// Every case runs the real `Session::run_plan` against a fresh broker,
        /// fresh peers and a fresh document (`run_sequence` builds them all).
        #[test]
        #[allow(
            clippy::too_many_lines,
            reason = "each case keeps its own fresh broker fixture and original assertions"
        )]
        fn run_plan_hands_on_only_the_exact_content_the_preceding_broker_step_produced() {
            // Case `run_plan_replaces_invented_content_with_the_exact_broker_result`.
            {
                let content = ContentStore::memory();
                let output = produced(&content);
                let hash = tilo_actions::content_hash(&output.resources[0])
                    .unwrap()
                    .to_owned();
                let observed = run_sequence(
                    &content,
                    &output,
                    &content_result(),
                    Between::Nothing,
                    false,
                );
                assert_eq!(observed.calls.len(), 2);
                assert_eq!(observed.calls[0], (PRODUCE.into(), serde_json::Map::new()));
                assert_eq!(
                    observed.calls[1],
                    (
                        "place-image".into(),
                        serde_json::Map::from_iter([(
                            tilo_actions::PROMPTED_PARAM.into(),
                            serde_json::Value::String(hash.clone())
                        ),])
                    )
                );
                assert_eq!(observed.images, vec![hash]);
                assert!(observed.instruction.is_empty());
            }

            // Case `run_plan_refuses_missing_broker_content_despite_a_held_photograph`.
            {
                let observed = run_sequence(
                    &ContentStore::memory(),
                    &ActionOutput::default(),
                    &ResultSpec::None,
                    Between::Nothing,
                    false,
                );
                assert_no_consumer(&observed, &[PRODUCE]);
            }

            // Case `run_plan_refuses_wrong_kind_despite_a_held_photograph`.
            {
                let observed = run_sequence(
                    &ContentStore::memory(),
                    &ActionOutput {
                        value: None,
                        resources: vec!["files.entry:fixture".into()],
                    },
                    &ResultSpec::Resource {
                        resource_kind: "files.entry".into(),
                    },
                    Between::Nothing,
                    false,
                );
                assert_no_consumer(&observed, &[PRODUCE]);
            }

            // Case `run_plan_refuses_ambiguous_broker_content`.
            {
                let content = ContentStore::memory();
                let mut output = produced(&content);
                output.resources.push(
                    tilo_actions::content_resource(&"a".repeat(64))
                        .expect("the duplicate has a valid content hash"),
                );
                let observed = run_sequence(
                    &content,
                    &output,
                    &content_result(),
                    Between::Nothing,
                    false,
                );
                assert_no_consumer(&observed, &[PRODUCE]);
            }

            // Case `run_plan_invalidates_content_after_a_nonproducing_broker_step`.
            {
                let content = ContentStore::memory();
                let observed = run_sequence(
                    &content,
                    &produced(&content),
                    &content_result(),
                    Between::BrokerView,
                    false,
                );
                assert_no_consumer(&observed, &[PRODUCE, "view"]);
            }

            // Case `run_plan_invalidates_content_after_a_skipped_duplicate`.
            {
                let content = ContentStore::memory();
                let observed = run_sequence(
                    &content,
                    &produced(&content),
                    &content_result(),
                    Between::Duplicate,
                    false,
                );
                assert_no_consumer(&observed, &[PRODUCE]);
            }

            // Case `run_plan_stops_after_a_failed_producer`.
            {
                let content = ContentStore::memory();
                let observed = run_sequence(
                    &content,
                    &produced(&content),
                    &content_result(),
                    Between::Nothing,
                    true,
                );
                assert_no_consumer(&observed, &[PRODUCE]);
                assert!(observed.instruction.contains("Producer refused"));
            }

            // Case `run_plan_reports_unavailable_bytes_without_inserting_an_image`.
            {
                let missing = "e".repeat(64);
                let content = ContentStore::memory();
                assert!(!content.has(&missing));
                let observed = run_sequence(
                    &content,
                    &ActionOutput {
                        value: None,
                        resources: vec![
                            tilo_actions::content_resource(&missing)
                                .expect("the missing blob still has a valid content hash"),
                        ],
                    },
                    &content_result(),
                    Between::Nothing,
                    false,
                );
                assert_eq!(observed.calls.len(), 2);
                assert_eq!(observed.calls[1].0, "place-image");
                assert_eq!(
                    observed.calls[1].1.get(tilo_actions::PROMPTED_PARAM),
                    Some(&serde_json::Value::String(missing)),
                );
                assert!(observed.images.is_empty());
                assert!(observed.instruction.contains("not available"));
            }
        }

        /// An operation the agent sees only in its own replica — the photo
        /// editor does not publish it through the broker — is refused, never
        /// applied to the replica behind the application's back. The chain
        /// stops there, so nothing consumes the earlier product either.
        #[test]
        fn run_plan_refuses_an_unpublished_local_edit_and_stops_the_chain() {
            let content = ContentStore::memory();
            let observed = run_sequence(
                &content,
                &produced(&content),
                &content_result(),
                Between::LocalRotate,
                false,
            );
            assert_eq!(
                observed.rotation, 0,
                "an operation the application does not publish must not run"
            );
            assert_no_consumer(&observed, &[PRODUCE]);
            assert!(
                observed.instruction.contains("not offering this action"),
                "{}",
                observed.instruction
            );
        }
    }
}
