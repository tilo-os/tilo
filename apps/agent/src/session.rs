//! One agent session: join the room, hold the replica, act on instructions.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ui::collaboration::{
    CertifiedEndpointIdentity, CollabEvent, CollabRequest, DocumentMutation, PresencePayload,
    RouteSpec,
};
use ui::permissions::PermissionContext;
use ui::presence::{PathSegment, PresenceKey};
use uuid::Uuid;
use word_processor::actions::{self as wp, RegionKind, WordProcessorAction};

use crate::apply::Editor;
use crate::model::{Chooser, Route};
use crate::plan::{self, Block, ConfirmationEvidence, Instruction, Plan};
use crate::providers::default_selection;
use crate::surface::{self, AppContract, View};
use crate::wire::Link;

const TASK_CATALOGUE_WAIT: Duration = Duration::from_secs(2);
const TASK_ACTION_WAIT: Duration = Duration::from_secs(30);

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
    output: qos_actions::ActionOutput,
    /// A local document's app-derived product. Local document actions predate
    /// `ActionOutput`; this preserves their existing honest hand-off without
    /// pretending the model or the generic action door produced it.
    product: Option<surface::Product>,
}

impl StepOutcome {
    fn content_hash(&self) -> Result<&str, String> {
        let mut hashes = self
            .output
            .resources
            .iter()
            .filter_map(|address| qos_actions::content_hash(address));
        if let Some(hash) = hashes.next() {
            if hashes.next().is_some() {
                return Err("the previous action produced more than one content resource".into());
            }
            return Ok(hash);
        }
        if let Some(surface::Product::Content { hash }) = &self.product {
            return Ok(hash);
        }
        Err("the previous action produced no content resource".into())
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
        qos_actions::PROMPTED_PARAM.to_string(),
        serde_json::Value::String(hash.to_string()),
    );
    Ok(arguments)
}

#[derive(Debug)]
enum TaskActionStop {
    Cancelled,
    Failed(String),
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
    let context = serde_json::from_value::<qos_actions::ActionContext>(run.action_context.clone())
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
        qos_actions::ConfirmationPolicy::None
    ) {
        return Err("Tasks cannot invoke an action requiring human confirmation".into());
    }
    if !offer.wants_content() {
        qos_actions::ActionArgs::bind_json(&offer.descriptor.params, &arguments)
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
    /// The agent's name: the caret pill, the history author, and the word
    /// instructions start with.
    pub name: String,
    /// Pause between edits, so people can watch the caret work.
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
    /// The directory the agent is browsing, for an app whose subject is
    /// the filesystem rather than a document.
    pub directory: Option<PathBuf>,
    /// What the agent may enumerate and perform. A denied operation is
    /// absent from what it discovers, never merely refused — see
    /// [`crate::surface::offers`].
    pub permissions: PermissionContext,
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
    /// When the agent last asked the room for the document. It asks again
    /// until it has one: an app that was still opening its file when the
    /// agent arrived never saw the first ask, and the agent would then wait
    /// for a reply that is never coming. On a loaded machine that is the
    /// difference between the beat happening and the beat hanging.
    asked_at: Instant,
    /// How the last instruction was decided. The record that keeps a run on
    /// the fallback from being indistinguishable from a live one afterwards.
    last_route: Option<Route>,
    /// Complete, permission-filtered live action directory supplied by the
    /// broker. This is the planner's application catalogue; local document
    /// contracts remain only for the agent's own editorial state/tools.
    broker_surfaces: Vec<ui::collaboration::BrokerActionSurface>,
    next_broker_request: u64,
    active_run: Option<ActiveRun>,
}

/// How often the agent asks again while it still has no document.
const REASK_EVERY: Duration = Duration::from_millis(750);

/// Ask the room for the document: peers already in it answer an `Arrived`
/// with their full state. Safe to repeat — a second copy of a document the
/// agent already holds merges to the same thing.
fn ask_for_document(link: &mut Link) -> Result<()> {
    link.send(&CollabRequest::Presence {
        key: PresenceKey::Workspace {
            id: "word-processor".to_string(),
        },
        payload: PresencePayload::Arrived,
    })
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
        let mut link = match &cfg.endpoint {
            Some(path) => Link::connect_endpoint(
                path,
                &cfg.app_id,
                cfg.cluster,
                cfg.record.clone(),
                Some(cfg.name.clone()),
                cfg.endpoint_identity.clone(),
            )?,
            None => Link::connect(
                &cfg.app_id,
                cfg.cluster,
                cfg.record.clone(),
                Some(cfg.name.clone()),
            )?,
        };
        // Discovery is room-wide, but writes never are. This subscription
        // lets the agent learn other applications' exported operations while
        // every mutation below still names its owning app route explicitly.
        link.join_route(RouteSpec::cluster(cfg.cluster));
        // The late-join handshake: every peer in the room answers an
        // `Arrived` with its full document.
        ask_for_document(&mut link)?;
        tracing::info!(
            transport = if cfg.endpoint.is_some() { "typed-endpoint" } else { "QOS_SVC_COLLAB_FD" },
            app = %cfg.app_id,
            room = %cfg.cluster,
            record = ?cfg.record,
            name = %cfg.name,
            "joined the room; waiting for the document"
        );
        let chooser = default_selection().map_or_else(
            || Chooser::new(false, cfg.model_timeout),
            |selection| Chooser::with_provider(cfg.model, cfg.model_timeout, selection),
        );
        // Said once, at the top of the run: an operator should know which
        // path a recording is going to take before the beat, not after.
        tracing::info!(
            live = chooser.is_live(),
            model = ?default_selection().map(|selection| selection.model.id),
            timeout_ms = cfg.model_timeout.as_millis() as u64,
            "action choice: {}",
            if chooser.is_live() {
                "a model chooses from the app's exported actions"
            } else {
                "no model available; the keyword vocabulary chooses"
            }
        );
        Ok(Self {
            cfg,
            link,
            editor: None,
            peers: BTreeMap::new(),
            handled: HashSet::new(),
            watching: HashMap::new(),
            dispatched_this_turn: HashSet::new(),
            chooser,
            last_route: None,
            asked_at: Instant::now(),
            broker_surfaces: Vec::new(),
            next_broker_request: 1,
            active_run: None,
        })
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
    /// `await_document` (see [`Self::unanswered`] for the message), or when
    /// a turn's own I/O — receiving from the broker, decoding a snapshot,
    /// applying an edit — fails.
    pub fn run(&mut self) -> Result<()> {
        let joined = Instant::now();
        loop {
            if self.step(Duration::from_millis(100))? && self.cfg.once {
                return Ok(());
            }
            if self.editor.is_none()
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
    pub fn run_task_runs(&mut self) -> Result<()> {
        tracing::info!(agent = %self.cfg.name, "waiting for broker-addressed task runs");
        loop {
            match self.link.recv(Duration::from_millis(100))? {
                Some(CollabEvent::ActionCatalogue { surfaces }) => {
                    self.broker_surfaces = surfaces;
                }
                Some(CollabEvent::TaskRun(run)) => self.run_task(run)?,
                Some(_) | None => {}
            }
        }
    }

    fn run_task(&mut self, run: runtime_collab::TaskRunEnvelope) -> Result<()> {
        if let Err(message) = validate_task_run(&run) {
            tracing::warn!(run_id = run.run_id, %message, "refused malformed task run");
            self.report_task(&run, runtime_collab::TaskRunUpdate::Failed { message })?;
            return Ok(());
        }
        self.report_task(&run, runtime_collab::TaskRunUpdate::Accepted)?;
        self.report_task(
            &run,
            runtime_collab::TaskRunUpdate::Progress {
                message: "Reviewing the permission-filtered action catalogue".into(),
            },
        )?;
        self.wait_for_task_catalogue()?;
        if self.task_cancelled(&run)? {
            return self.cancel_task(&run, "Cancelled before planning");
        }
        let actions = match self.plan_task(&run) {
            Ok(actions) if !actions.is_empty() => actions,
            Ok(_) => {
                return self.fail_task(&run, "No permitted live action can complete this task");
            }
            Err(message) => return self.fail_task(&run, &message),
        };
        let mut previous = None;
        for (index, action) in actions.iter().enumerate() {
            if self.task_cancelled(&run)? {
                return self.cancel_task(&run, "Cancelled before the next action");
            }
            let arguments =
                match arguments_for_step(action.carries, &action.arguments, previous.as_ref()) {
                    Ok(arguments) => arguments,
                    Err(message) => return self.fail_task(&run, &message),
                };
            self.report_task(
                &run,
                runtime_collab::TaskRunUpdate::Progress {
                    message: format!("Running {}/{} · {}", index + 1, actions.len(), action.label),
                },
            )?;
            match self.invoke_task_action(&run, action, &arguments)? {
                Ok(outcome) => previous = Some(outcome),
                Err(TaskActionStop::Cancelled) => {
                    return self.cancel_task(&run, "Cancelled while an action was running");
                }
                Err(TaskActionStop::Failed(message)) => return self.fail_task(&run, &message),
            }
        }
        self.report_task(
            &run,
            runtime_collab::TaskRunUpdate::Succeeded {
                message: format!("Completed {} broker-approved action(s)", actions.len()),
            },
        )
    }

    fn report_task(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        update: runtime_collab::TaskRunUpdate,
    ) -> Result<()> {
        self.link.send(&CollabRequest::ReportTaskRun {
            run_id: run.run_id,
            lease: run.lease.clone(),
            update,
        })
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
        let deadline = Instant::now() + TASK_CATALOGUE_WAIT;
        let mut deferred = Vec::new();
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.link.recv(remaining.min(Duration::from_millis(100)))? {
                Some(CollabEvent::ActionCatalogue { surfaces }) => {
                    self.broker_surfaces = surfaces;
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

    fn task_cancelled(&mut self, run: &runtime_collab::TaskRunEnvelope) -> Result<bool> {
        let request_id = self.next_request_id();
        self.link.send(&CollabRequest::TaskRunCancellation {
            request_id,
            run_id: run.run_id,
            lease: run.lease.clone(),
        })?;
        self.wait_for_task_cancellation(request_id)
    }

    fn wait_for_task_cancellation(&mut self, request_id: u64) -> Result<bool> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut deferred = Vec::new();
        loop {
            let now = Instant::now();
            if now >= deadline {
                self.link.defer(deferred);
                return Err(anyhow::anyhow!(
                    "the broker did not answer task cancellation status"
                ));
            }
            match self
                .link
                .recv((deadline - now).min(Duration::from_millis(100)))?
            {
                Some(CollabEvent::TaskRunCancellation {
                    request_id: received,
                    result,
                }) if received == request_id => {
                    self.link.defer(deferred);
                    return result.map_err(anyhow::Error::msg);
                }
                Some(CollabEvent::ActionCatalogue { surfaces }) => self.broker_surfaces = surfaces,
                Some(event) => deferred.push(event),
                None => {}
            }
        }
    }

    fn plan_task(&self, run: &runtime_collab::TaskRunEnvelope) -> Result<Vec<TaskAction>, String> {
        let offers = surface::broker_offers(&self.broker_surfaces)
            .into_iter()
            .filter(|offer| {
                offer.broker_peer_id.is_some()
                    && matches!(
                        offer.descriptor.contract.confirmation,
                        qos_actions::ConfirmationPolicy::None
                    )
            })
            .collect::<Vec<_>>();
        match run.planning {
            runtime_collab::TaskRunPlanning::RulesOnly => {
                let (offer, arguments) =
                    surface::from_verbs(&run.title, &offers).map_err(|error| {
                        format!("Rules-only planner could not choose an action: {error:?}")
                    })?;
                task_action_from_offer(offer, arguments).map(|action| vec![action])
            }
            runtime_collab::TaskRunPlanning::Model => {
                let catalogue = surface::Catalogue {
                    offers: offers.clone(),
                    facts: Vec::new(),
                    held: Vec::new(),
                };
                match self.chooser.choose_actions(&run.title, &[], &catalogue, 8) {
                    Ok(answer) => answer
                        .actions
                        .iter()
                        .zip(&answer.arguments)
                        .map(|(id, arguments)| {
                            let instruction =
                                Instruction::from_answer(id, &run.title, Some(arguments), &offers)
                                    .map_err(|error| {
                                        format!("Model selected an unavailable action: {error:?}")
                                    })?;
                            let Instruction::Perform {
                                id,
                                arguments,
                                confirmation,
                            } = instruction
                            else {
                                return Err("Model task plans must name broker-published actions"
                                    .to_string());
                            };
                            if !matches!(confirmation, ConfirmationEvidence::None) {
                                return Err(
                                    "An unattended task cannot satisfy an interactive confirmation"
                                        .to_string(),
                                );
                            }
                            let offer = surface::find(&offers, &id).ok_or_else(|| {
                                "The planned action is no longer published".to_string()
                            })?;
                            task_action_from_offer(offer, arguments)
                        })
                        .collect(),
                    Err(route) => {
                        tracing::info!(reason = %route.detail(), "model unavailable; using rules-only task planning");
                        let (offer, arguments) =
                            surface::from_verbs(&run.title, &offers).map_err(|error| {
                                format!("Model fallback could not choose an action: {error:?}")
                            })?;
                        task_action_from_offer(offer, arguments).map(|action| vec![action])
                    }
                }
            }
        }
    }

    fn invoke_task_action(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        action: &TaskAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Result<StepOutcome, TaskActionStop>> {
        let request_id = self.next_request_id();
        self.link.send(&CollabRequest::InvokeTaskAction {
            request_id,
            run_id: run.run_id,
            lease: run.lease.clone(),
            peer_id: action.peer_id,
            resource: action.resource.clone(),
            action: action.action.clone(),
            arguments: arguments.clone(),
        })?;
        let (id, mut update) = match self.wait_for_task_action_result(request_id)? {
            Ok(result) => result,
            Err(error) => return Ok(Err(TaskActionStop::Failed(error))),
        };
        let mut interaction_sequence = 0_u64;
        self.publish_task_interaction(run, action, &update, interaction_sequence);
        let deadline = Instant::now() + TASK_ACTION_WAIT;
        while !update.is_terminal() && Instant::now() < deadline {
            if self.task_cancelled(run)? {
                let cancel_request = self.next_request_id();
                self.link.send(&CollabRequest::CancelPublishedInvocation {
                    request_id: cancel_request,
                    id,
                })?;
                let _ = self.wait_for_broker_result(cancel_request)?;
                interaction_sequence = interaction_sequence.saturating_add(1);
                let cancelled = qos_actions::InvocationUpdate::Cancelled {
                    invocation: update.invocation(),
                    message: Some("Cancelled from Task Center".into()),
                };
                self.publish_task_interaction(run, action, &cancelled, interaction_sequence);
                return Ok(Err(TaskActionStop::Cancelled));
            }
            std::thread::sleep(Duration::from_millis(50));
            let request_id = self.next_request_id();
            self.link
                .send(&CollabRequest::PublishedInvocationStatus { request_id, id })?;
            update = match self.wait_for_broker_result(request_id)? {
                Ok((_, update)) => update,
                Err(error) => return Ok(Err(TaskActionStop::Failed(error))),
            };
            interaction_sequence = interaction_sequence.saturating_add(1);
            self.publish_task_interaction(run, action, &update, interaction_sequence);
        }
        match update {
            qos_actions::InvocationUpdate::Succeeded { output, .. } => Ok(Ok(StepOutcome {
                output,
                product: self.product_for_app(&action.app),
            })),
            qos_actions::InvocationUpdate::Cancelled { .. } => Ok(Err(TaskActionStop::Cancelled)),
            qos_actions::InvocationUpdate::Failed { error, .. } => {
                Ok(Err(TaskActionStop::Failed(error.message)))
            }
            other => {
                let message = "Task action did not reach a terminal state before its deadline";
                interaction_sequence = interaction_sequence.saturating_add(1);
                self.publish_task_interaction(
                    run,
                    action,
                    &qos_actions::InvocationUpdate::Failed {
                        invocation: other.invocation(),
                        error: qos_actions::ActionFailure {
                            code: "timeout".into(),
                            message: message.into(),
                            details: None,
                            retryable: true,
                        },
                    },
                    interaction_sequence,
                );
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
        update: &qos_actions::InvocationUpdate,
        sequence: u64,
    ) {
        let phase = match update {
            qos_actions::InvocationUpdate::Accepted { .. } => {
                qos_actions::InteractionPhase::Applying
            }
            qos_actions::InvocationUpdate::Progress {
                phase,
                completed,
                total,
                ..
            } => qos_actions::InteractionPhase::Progress {
                label: phase.clone(),
                completed: *completed,
                total: *total,
            },
            qos_actions::InvocationUpdate::Item { .. } => qos_actions::InteractionPhase::Applying,
            qos_actions::InvocationUpdate::Succeeded { .. } => {
                qos_actions::InteractionPhase::Committed
            }
            qos_actions::InvocationUpdate::Failed { error, .. } => {
                qos_actions::InteractionPhase::Failed {
                    code: Some(error.code.clone()),
                    message: error.message.clone(),
                }
            }
            qos_actions::InvocationUpdate::Cancelled { message, .. } => {
                qos_actions::InteractionPhase::Cancelled {
                    message: message.clone(),
                }
            }
        };
        let context =
            serde_json::from_value::<qos_actions::ActionContext>(run.action_context.clone()).ok();
        let interaction = qos_actions::SemanticInteraction {
            version: qos_actions::INTERACTION_VERSION,
            invocation: update.invocation(),
            sequence,
            // The local broker replaces these fields from the authenticated
            // admission before fan-out. They are populated only so the
            // strongly typed value is valid before crossing that boundary.
            actor: qos_actions::InteractionActor {
                id: self.cfg.name.clone(),
                kind: qos_actions::InteractionActorKind::Agent,
            },
            phase,
            action: qos_actions::InteractionAction {
                id: action.action.clone(),
                label: action.label.clone(),
                app: Some(qos_actions::ActionApp {
                    id: action.app.clone(),
                    label: action.app.clone(),
                }),
            },
            resource: qos_actions::ActionResource {
                id: action.resource.clone(),
                kind: action.resource_kind.clone(),
                label: action.resource_label.clone(),
            },
            anchor: if let Some(id) = action.resource.strip_prefix("ui:") {
                qos_actions::InteractionAnchor::SemanticControl { id: id.into() }
            } else if action.resource_kind.contains("entry")
                || action.resource_kind.contains("file")
            {
                qos_actions::InteractionAnchor::ListItems {
                    ids: vec![action.resource.clone()],
                }
            } else {
                qos_actions::InteractionAnchor::Resource
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
    ) -> Result<Result<(u64, qos_actions::InvocationUpdate), String>> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut deferred = Vec::new();
        loop {
            let now = Instant::now();
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
                Some(CollabEvent::ActionCatalogue { surfaces }) => self.broker_surfaces = surfaces,
                Some(event @ CollabEvent::Document { .. }) => {
                    match event
                        .document_mutation()
                        .transpose()?
                        .ok_or_else(|| anyhow::anyhow!("document event lost its mutation"))?
                    {
                        DocumentMutation::Snapshot(data) => {
                            let _ = self.absorb_snapshot(&data)?;
                        }
                        DocumentMutation::Incremental(data) => {
                            let _ = self.merge_incremental_anywhere(&data)?;
                        }
                    }
                }
                Some(event) => deferred.push(event),
                None => {}
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
    /// # Errors
    ///
    /// Whenever a turn's own I/O fails — see [`Self::run`].
    pub fn run_until(&mut self, deadline: Instant) -> Result<bool> {
        let mut acted = false;
        while Instant::now() < deadline {
            acted |= self.step(Duration::from_millis(50))?;
            if acted && self.cfg.once {
                break;
            }
        }
        Ok(acted)
    }

    /// One turn: absorb whatever the room sent, then act if the document
    /// asks. Returns whether an instruction was carried out.
    fn step(&mut self, wait: Duration) -> Result<bool> {
        let mut changed = false;
        // Still empty-handed: ask again. An app that was still opening its
        // file when the agent arrived never saw the first ask, and nothing
        // else would ever prompt a reply.
        if self.editor.is_none() && self.asked_at.elapsed() >= REASK_EVERY {
            ask_for_document(&mut self.link)?;
            self.asked_at = Instant::now();
            tracing::debug!("asked the room for the document again");
        }
        match self.link.recv(wait)? {
            Some(event @ CollabEvent::Document { .. }) => {
                changed = match event
                    .document_mutation()
                    .transpose()?
                    .ok_or_else(|| anyhow::anyhow!("document event lost its mutation"))?
                {
                    DocumentMutation::Snapshot(data) => self.absorb_snapshot(&data)?,
                    DocumentMutation::Incremental(data) => {
                        self.merge_incremental_anywhere(&data)?
                    }
                };
            }
            Some(CollabEvent::RemotePresenceUpdate {
                user_id,
                payload: PresencePayload::Arrived,
                ..
            }) => {
                // A participant arriving after us deserves what we hold —
                // every document, not only the one we were invited to.
                if let Some(ed) = self.editor.as_mut() {
                    let full = ed.full();
                    self.link
                        .send(&CollabRequest::DocumentSnapshot { data: full })?;
                }
                for peer in self.peers.values_mut() {
                    let full = peer.full();
                    self.link.send_to_route(
                        RouteSpec::app(peer.contract().app_id, self.cfg.cluster),
                        &CollabRequest::DocumentSnapshot { data: full },
                    )?;
                }
                tracing::debug!(%user_id, peers = self.peers.len(), "sent our documents to a new participant");
            }
            Some(CollabEvent::ActionCatalogue { surfaces }) => {
                self.broker_surfaces = surfaces;
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
        if !changed {
            return Ok(false);
        }
        self.act()
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
    fn absorb_snapshot(&mut self, data: &[u8]) -> Result<bool> {
        let Some(ed) = self.editor.as_mut() else {
            if !crate::apply::looks_like_a_document(data) {
                // Not a page this agent authors into — but it may still be
                // another application's room state, which belongs in
                // `peers` rather than being dropped outright.
                return self.absorb_peer(data);
            }
            self.editor = Some(Editor::from_snapshot(data, &self.cfg.name)?);
            tracing::info!(bytes = data.len(), "received the document");
            return Ok(true);
        };
        if ed.merge_full(data)? {
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
        self.absorb_peer(data)
    }

    /// Take a peer's whole-document frame as a *second* application's
    /// state — one this session was not invited to, but whose operations
    /// [`Self::offers`] can still discover and [`Self::perform`] can still
    /// dispatch, once its own contract recognises it.
    fn absorb_peer(&mut self, data: &[u8]) -> Result<bool> {
        let Some(contract) = crate::apply::attribute(data) else {
            tracing::debug!(
                bytes = data.len(),
                "ignored a peer's state: no app here claims it"
            );
            return Ok(false);
        };
        if let Some(peer) = self.peers.get_mut(contract.namespace) {
            return peer.merge_full_any(data);
        }
        let peer = Editor::peer_from_snapshot(contract, data, &self.cfg.name)?;
        tracing::info!(
            app = contract.namespace,
            bytes = data.len(),
            "a second application's document joined the room"
        );
        self.peers.insert(contract.namespace, peer);
        Ok(true)
    }

    /// Apply a peer's incremental change to whichever replica it belongs
    /// to — the primary document first, since that is the common case,
    /// then each held peer. A delta belonging to a document this session
    /// holds neither of is silently not this agent's business.
    fn merge_incremental_anywhere(&mut self, data: &[u8]) -> Result<bool> {
        if let Some(ed) = self.editor.as_mut()
            && ed.merge_incremental(data)?
        {
            return Ok(true);
        }
        for peer in self.peers.values_mut() {
            if peer.merge_incremental(data)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Every document this session holds, primary first, as the pairs
    /// [`surface::offers`] and [`surface::facts`] are built from.
    fn held(&self) -> Vec<(&'static AppContract, &automerge::AutoCommit)> {
        let mut out = Vec::new();
        if let Some(ed) = self.editor.as_ref() {
            out.push((ed.contract(), &ed.doc));
        }
        for peer in self.peers.values() {
            out.push((peer.contract(), &peer.doc));
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
        view: &View,
    ) -> Vec<surface::Offer> {
        let mut offers = Self::document_broker_offers(&self.broker_surfaces, &self.cfg.permissions);
        for local in surface::offers(held, view, &self.cfg.permissions) {
            if !offers
                .iter()
                .any(|broker| broker.address() == local.address())
            {
                offers.push(local);
            }
        }
        offers
    }

    /// Broker authorization is necessary but the invitation carried by this
    /// document session remains an independent upper bound. Task leases use
    /// the broker catalogue directly; document instructions use the
    /// intersection so a stale or narrower launch grant cannot be widened by
    /// a broader workspace catalogue.
    fn document_broker_offers(
        surfaces: &[ui::collaboration::BrokerActionSurface],
        permissions: &PermissionContext,
    ) -> Vec<surface::Offer> {
        surface::broker_offers(surfaces)
            .into_iter()
            .filter(|offer| surface::permitted_descriptor(permissions, &offer.descriptor))
            .collect()
    }

    /// Where the agent is looking: `focus` on a document, and the
    /// directory it was configured to browse, when it has one.
    fn view(&self, focus: Option<String>) -> View {
        View {
            focus_block: focus.clone(),
            focus_offset: 0,
            target_blocks: focus.into_iter().collect(),
            format_spans: Vec::new(),
            region: word_processor::actions::RegionKind::Body,
            subject: None,
            directory: self.cfg.directory.clone(),
        }
    }

    /// Perform the discovered operation `id` names, against whichever
    /// document holds it. `Ok(Err(_))` is a refusal in words a person
    /// reads; `Err(_)` is this session's own I/O failing.
    ///
    /// Resolved against a tree rebuilt from the document **right now**,
    /// through [`Editor::invoke`] — never against the survey `id` was
    /// chosen from. An operation that has gone away since is a refusal
    /// here, not a substitution.
    fn perform(
        &mut self,
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmation: &ConfirmationEvidence,
    ) -> Result<Result<StepOutcome, String>> {
        if let Some(offer) = self.wait_for_broker_offer(id)? {
            return self.perform_broker(id, &offer, arguments, confirmation);
        }
        self.perform_local(id, arguments, confirmation)
    }

    /// Give the broker's independently delivered catalogue stream a chance
    /// to catch up with the document stream before falling back to local
    /// execution. An app publishes its actions before its snapshot, but the
    /// two replies travel on separate Cap'n Proto streams and therefore have
    /// no cross-stream ordering guarantee.
    fn wait_for_broker_offer(&mut self, id: &str) -> Result<Option<surface::Offer>> {
        // A broker-prefixed id pins an exact live publisher. Only a local
        // compatibility offer may be upgraded to the matching broker route;
        // never silently retarget a vanished broker selection to another app.
        let wanted = (!id.starts_with("broker:"))
            .then(|| surface::decode_address(id))
            .flatten();
        let permissions = self.cfg.permissions.clone();
        let find = |surfaces: &[ui::collaboration::BrokerActionSurface]| {
            let offers = Self::document_broker_offers(surfaces, &permissions);
            surface::find(&offers, id).cloned().or_else(|| {
                wanted
                    .as_ref()
                    .and_then(|address| offers.into_iter().find(|offer| offer.address() == address))
            })
        };
        if let Some(offer) = find(&self.broker_surfaces) {
            return Ok(Some(offer));
        }
        let deadline = Instant::now() + TASK_CATALOGUE_WAIT;
        let mut deferred = Vec::new();
        let mut found = None;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.link.recv(remaining.min(Duration::from_millis(100)))? {
                Some(CollabEvent::ActionCatalogue { surfaces }) => {
                    self.broker_surfaces = surfaces;
                    if let Some(offer) = find(&self.broker_surfaces) {
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

    /// What the named application's current document denotes after an
    /// action. This remains the application's function, not an interpretation
    /// of its document by the agent.
    fn product_for_app(&self, app: &str) -> Option<surface::Product> {
        let namespace = surface::for_app_id(app).map_or(app, |contract| contract.namespace);
        let view = self.view(None);
        self.editor
            .as_ref()
            .filter(|editor| editor.contract().namespace == namespace)
            .and_then(|editor| (editor.contract().product)(&view, &editor.doc))
            .or_else(|| {
                self.peers
                    .get(namespace)
                    .and_then(|editor| (editor.contract().product)(&view, &editor.doc))
            })
    }

    fn perform_broker(
        &mut self,
        id: &str,
        offer: &surface::Offer,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmation: &ConfirmationEvidence,
    ) -> Result<Result<StepOutcome, String>> {
        let Some(peer_id) = offer.broker_peer_id else {
            return Ok(Err(
                "that operation is local-only and cannot be brokered".to_string()
            ));
        };
        let descriptor = offer.descriptor.clone();
        if !confirmation.satisfies(&descriptor.contract.confirmation) {
            return Ok(Err("that action requires explicit confirmation".to_string()));
        }
        if let Err(error) = qos_actions::ActionArgs::bind_json(&descriptor.params, arguments) {
            return Ok(Err(format!("I couldn't do that - {error}")));
        }
        let confirmation_json = match confirmation {
            ConfirmationEvidence::None => serde_json::json!({}),
            ConfirmationEvidence::Confirm => serde_json::json!({"confirm": true}),
            ConfirmationEvidence::Phrase(phrase) => {
                serde_json::json!({"confirmation_phrase": phrase})
            }
        };
        let request_id = self.next_request_id();
        if descriptor.resource_kind == surface::APPLICATION_KIND
            && descriptor.address.action == surface::OPEN_ACTION
        {
            self.link.send(&CollabRequest::LaunchPublishedApplication {
                request_id,
                app_id: offer.app.clone(),
            })?;
        } else {
            self.link.send(&CollabRequest::InvokePublishedAction {
                request_id,
                peer_id,
                resource: descriptor.address.resource.clone(),
                action: descriptor.address.action,
                arguments: arguments.clone(),
                confirmation: confirmation_json,
            })?;
        }
        let (invocation_id, mut update) = match self.wait_for_broker_result(request_id)? {
            Ok(receipt) => receipt,
            Err(error) => return Ok(Err(format!("I couldn't do that - {error}"))),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while !update.is_terminal() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
            let request_id = self.next_request_id();
            self.link.send(&CollabRequest::PublishedInvocationStatus {
                request_id,
                id: invocation_id,
            })?;
            match self.wait_for_broker_result(request_id)? {
                Ok((_, latest)) => update = latest,
                Err(error) => return Ok(Err(format!("I couldn't observe that action - {error}"))),
            }
        }
        if !update.is_terminal() {
            return Ok(Err(format!(
                "action {invocation_id} was admitted but did not reach a terminal state in time"
            )));
        }
        match update {
            qos_actions::InvocationUpdate::Failed { error, .. } => {
                Ok(Err(format!("I couldn't do that - {}", error.message)))
            }
            qos_actions::InvocationUpdate::Cancelled { message, .. } => {
                Ok(Err(message.unwrap_or_else(|| {
                    "that action was cancelled".to_string()
                })))
            }
            qos_actions::InvocationUpdate::Succeeded { output, .. } => {
                tracing::info!(%id, invocation_id, ?output, "broker action completed");
                std::thread::sleep(self.cfg.pace);
                Ok(Ok(StepOutcome {
                    output,
                    product: self.product_for_app(&offer.app),
                }))
            }
            _ => Ok(Err(
                "the broker returned an invalid nonterminal result".to_string()
            )),
        }
    }

    fn perform_local(
        &mut self,
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmation: &ConfirmationEvidence,
    ) -> Result<Result<StepOutcome, String>> {
        let context = self.task_context();
        let Some(address) = surface::decode_address(id) else {
            return Ok(Err("that no longer names anything I can find".to_string()));
        };
        let namespace = surface::app_of(&address.resource).to_string();
        let view = self.view(None);
        let registry = surface::registry();
        let is_primary = self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.contract().namespace == namespace);
        let held = if is_primary {
            self.editor.as_mut()
        } else {
            self.peers.get_mut(namespace.as_str())
        };
        let Some(editor) = held else {
            return Ok(Err("that isn't open here, so I can't act on it".to_string()));
        };
        let target_app_id = editor.contract().app_id;
        editor.set_history_context(context);
        let before = editor.latest_entry_id();
        let live = editor.resources(&view);
        let Some(descriptor) = qos_actions::door::survey(&live)
            .into_iter()
            .find(|descriptor| descriptor.address == address)
        else {
            return Ok(Err("that no longer names anything I can find".to_string()));
        };
        if !surface::permitted_descriptor(&self.cfg.permissions, &descriptor) {
            return Ok(Err("that no longer names anything I can find".to_string()));
        }
        if !confirmation.satisfies(&descriptor.contract.confirmation) {
            return Ok(Err("that action requires explicit confirmation".to_string()));
        }
        let invoked = if confirmation.is_confirmed() {
            editor.invoke_json_confirmed(&view, &address, arguments, &registry)
        } else {
            editor.invoke_json(&view, &address, arguments, &registry)
        };
        match invoked {
            Ok(Some(delta)) => {
                let end = editor.latest_entry_id();
                let product = (editor.contract().product)(&view, &editor.doc);
                let record = self.cfg.record.clone().unwrap_or_default();
                let request = CollabRequest::DocumentIncremental { data: delta };
                if is_primary {
                    self.link.send(&request)?;
                } else {
                    self.link
                        .send_to_route(RouteSpec::app(target_app_id, self.cfg.cluster), &request)?;
                }
                tracing::info!(%id, "performed a discovered operation");
                if let Some(end) = end
                    && Some(end) != before
                {
                    self.capture_step(target_app_id, &record, descriptor.action_label, end);
                }
                std::thread::sleep(self.cfg.pace);
                Ok(Ok(StepOutcome {
                    output: qos_actions::ActionOutput::default(),
                    product,
                }))
            }
            Ok(None) => {
                tracing::warn!(%id, "the operation changed nothing");
                Ok(Err("that didn't change anything".to_string()))
            }
            Err(error) => {
                tracing::warn!(%id, "refused: {error}");
                Ok(Err(format!("I couldn't do that - {error}")))
            }
        }
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_broker_request;
        self.next_broker_request = self.next_broker_request.saturating_add(1);
        id
    }

    fn wait_for_broker_result(
        &mut self,
        request_id: u64,
    ) -> Result<Result<(u64, qos_actions::InvocationUpdate), String>> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut deferred = Vec::new();
        loop {
            let now = Instant::now();
            if now >= deadline {
                self.link.defer(deferred);
                return Ok(Err(
                    "the broker did not answer the action request".to_string()
                ));
            }
            match self
                .link
                .recv((deadline - now).min(Duration::from_millis(100)))?
            {
                Some(CollabEvent::PublishedInvocationResult {
                    request_id: received,
                    result,
                }) if received == request_id => {
                    self.link.defer(deferred);
                    return Ok(result);
                }
                Some(CollabEvent::ActionCatalogue { surfaces }) => {
                    self.broker_surfaces = surfaces;
                }
                Some(event @ CollabEvent::Document { .. }) => {
                    match event
                        .document_mutation()
                        .transpose()?
                        .ok_or_else(|| anyhow::anyhow!("document event lost its mutation"))?
                    {
                        DocumentMutation::Snapshot(data) => {
                            let _ = self.absorb_snapshot(&data)?;
                        }
                        DocumentMutation::Incremental(data) => {
                            let _ = self.merge_incremental_anywhere(&data)?;
                        }
                    }
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
            Instant::now(),
            self.cfg.settle,
        ) {
            self.watching
                .insert(ins_id, (instruction_block.text.clone(), Instant::now()));
            return Ok(false);
        }
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
        qdoc::history::versions(&mut editor.doc)
            .into_iter()
            .rev()
            .find(|version| {
                !qdoc::history::is_schema_genesis(version)
                    && !version.actor_hex.eq_ignore_ascii_case(&mine)
            })
            .map(|version| version.actor_hex)
    }

    fn task_context(&self) -> Option<qos_actions::ActionContext> {
        let run = self.active_run.as_ref()?;
        let record = self.cfg.record.as_deref().unwrap_or("active document");
        let mut context =
            qos_actions::ActionContext::for_resource("Agent task", record, "document", record)
                .with_app(&self.cfg.app_id, "Agent")
                .with_purpose(&run.instruction)
                .with_group(&run.group_id, &run.instruction, "agent-run");
        if let Some(issuer) = &run.issued_by {
            context = context.with_facet(qos_actions::ActionFacet::new("assigned-by", issuer));
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
        let by = editor.actor_hex();
        if ui::record_run_with_details(
            &mut editor.doc,
            &run.instruction,
            Some(&by),
            run.issued_by.as_deref(),
            Some(&run.instruction),
            Some(&run.decision),
            &run.steps,
        )
        .is_some()
        {
            qdoc::history::commit_stamped(&mut editor.doc);
            let data = editor.doc.save_incremental();
            if !data.is_empty() {
                self.link
                    .send(&CollabRequest::DocumentIncremental { data })?;
            }
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
        let view = self.view(Some(ins_id.to_string()));
        let catalogue = surface::Catalogue {
            offers: self.offers(&held, &view),
            facts: surface::facts(&held, &view),
            held: held
                .iter()
                .map(|(contract, _)| contract.namespace)
                .collect(),
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
            let Some(at) = blocks.iter().position(|b| b.id == ins_id) else {
                // A step earlier in this very plan already answered the
                // instruction and took the paragraph with it.
                break;
            };
            let mut outcome = None;
            let applied = match &step.instruction {
                Instruction::Table => self.make_table(&blocks, at)?,
                Instruction::Caption => self.caption_image(&blocks, at)?,
                Instruction::Heading => {
                    self.make_heading(&blocks, at)?;
                    true
                }
                // A surveyed operation, named by address. Guarded by
                // `dispatched_this_turn`: a clause asked again mid-plan can
                // come back naming a step this turn already performed, and
                // without the guard "put it in the open document" placed
                // the same image twice.
                Instruction::Perform {
                    id,
                    arguments,
                    confirmation,
                } => {
                    if self.dispatched_this_turn.contains(id) {
                        true
                    } else {
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
                        let id = id.clone();
                        match self.perform(&id, &arguments, confirmation)? {
                            Ok(performed) => {
                                self.dispatched_this_turn.insert(id);
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

    /// Bullets above the instruction become a table. Returns whether it did
    /// — `false` when there were none to work with, in which case a reply
    /// saying so was written in place of doing anything.
    fn make_table(&mut self, blocks: &[Block], at: usize) -> Result<bool> {
        let bullets = plan::bullets_above(blocks, at);
        if bullets.is_empty() {
            let reply = format!(
                "{}: I looked for bullets above this line and found none.",
                self.cfg.name
            );
            let Some(instruction_block) = blocks.get(at) else {
                return Ok(false);
            };
            self.splice_reply(blocks, &instruction_block.id, &reply)?;
            return Ok(false);
        }
        let lines: Vec<&str> = bullets
            .iter()
            .filter_map(|&i| blocks.get(i).map(|b| b.text.as_str()))
            .collect();
        let rows = plan::table_cells(&lines);
        let cols = rows.first().map_or(1, Vec::len).max(1);
        let Some(last_bullet_block) = bullets.last().and_then(|&i| blocks.get(i)) else {
            // `bullets` was already shown non-empty above.
            return Ok(false);
        };
        let last_bullet = last_bullet_block.id.clone();
        let before: HashSet<String> = plan::table_ids(blocks).into_iter().collect();

        self.edit(
            &last_bullet,
            &wp::insert_table_action(&last_bullet, rows.len(), cols),
        )?;

        // The table's cells were minted inside the action; read them back.
        let fresh = plan::blocks(&self.editor.as_ref().context("no document")?.snapshot()?);
        let table_id = plan::table_ids(&fresh)
            .into_iter()
            .find(|t| !before.contains(t))
            .context("the inserted table's cells were not found")?;
        let cells = plan::cells_of(&fresh, &table_id);
        for (row, col, cell_id) in cells {
            let text = rows
                .get(row as usize)
                .and_then(|r| r.get(col as usize))
                .cloned()
                .unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            self.edit(
                &cell_id,
                &wp::splice_text_action(RegionKind::Body, &cell_id, 0, 0, &text),
            )?;
        }
        let gone: Vec<String> = bullets
            .iter()
            .filter_map(|&i| blocks.get(i).map(|b| b.id.clone()))
            .collect();
        self.edit(&last_bullet, &wp::remove_blocks_action(gone))?;
        Ok(true)
    }

    /// A caption paragraph under the first image. Returns whether there was
    /// one — `false` when there was nothing to caption, in which case a
    /// reply saying so was written in place of doing anything.
    fn caption_image(&mut self, blocks: &[Block], at: usize) -> Result<bool> {
        let Some(img) = plan::first_image(blocks) else {
            let reply = format!(
                "{}: there is no image in this document to caption.",
                self.cfg.name
            );
            let Some(instruction_block) = blocks.get(at) else {
                return Ok(false);
            };
            self.splice_reply(blocks, &instruction_block.id, &reply)?;
            return Ok(false);
        };
        let Some(img_block) = blocks.get(img) else {
            return Ok(false);
        };
        let img_id = img_block.id.clone();
        self.edit(&img_id, &wp::insert_paragraph_action(&img_id, "Figure 1"))?;
        Ok(true)
    }

    /// A structural pass over a rough draft: the first line becomes the
    /// title, each section label under it becomes a heading. One
    /// `SetParagraphStyle` per level, so the sections land together rather
    /// than one flicker at a time. An empty draft — nothing to find a
    /// title in — leaves the document untouched; this step always counts
    /// as done either way.
    fn make_heading(&mut self, blocks: &[Block], at: usize) -> Result<()> {
        let found = plan::structure(blocks, at);
        let Some(title_id) = found.title else {
            return Ok(());
        };
        self.edit(
            &title_id,
            &wp::set_paragraph_style_action(vec![title_id.clone()], "heading", Some(1), None),
        )?;
        if let Some(anchor) = found.sections.first().cloned() {
            self.edit(
                &anchor,
                &wp::set_paragraph_style_action(found.sections.clone(), "heading", Some(2), None),
            )?;
        }
        tracing::info!(sections = found.sections.len(), "made the draft a document");
        Ok(())
    }

    /// Show the caret on `focus`, apply `action`, broadcast the change, and
    /// pause so the room can see it happen.
    fn edit(&mut self, focus: &str, action: &WordProcessorAction) -> Result<()> {
        let before = self.editor.as_mut().and_then(Editor::latest_entry_id);
        let invocation = qos_actions::InvocationId::new();
        let resource_id = "word-processor.document".to_string();
        let agent_name = self.cfg.name.clone();
        let app_id = self.cfg.app_id.clone();
        let resource_label = self
            .cfg
            .record
            .clone()
            .unwrap_or_else(|| "Open document".into());
        let group = self.task_context().and_then(|context| context.group);
        let semantic = |phase, sequence| qos_actions::SemanticInteraction {
            version: qos_actions::INTERACTION_VERSION,
            invocation,
            sequence,
            actor: qos_actions::InteractionActor {
                id: agent_name.clone(),
                kind: qos_actions::InteractionActorKind::Agent,
            },
            phase,
            action: qos_actions::InteractionAction {
                id: "word-processor.edit".into(),
                label: describe(action),
                app: Some(qos_actions::ActionApp {
                    id: app_id.clone(),
                    label: "Word Processor".into(),
                }),
            },
            resource: qos_actions::ActionResource {
                id: resource_id.clone(),
                kind: "word-processor.document".into(),
                label: resource_label.clone(),
            },
            anchor: word_action_anchor(action, focus),
            group: group.clone(),
        };
        let interaction_key =
            PresenceKey::resource(&self.cfg.app_id, &resource_id, "word-processor.document");
        let _ = self.link.send(&CollabRequest::Presence {
            key: interaction_key.clone(),
            payload: PresencePayload::Interaction(Box::new(semantic(
                qos_actions::InteractionPhase::Applying,
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
        let delta = self
            .editor
            .as_mut()
            .context("no document yet")?
            .apply(action)?;
        if let Some(data) = delta {
            self.link
                .send(&CollabRequest::DocumentIncremental { data })?;
            if let WordProcessorAction::SpliceText { at, ins, .. } = action
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
                    qos_actions::InteractionPhase::Committed,
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
                    qos_actions::InteractionPhase::Failed {
                        code: Some("no_change".into()),
                        message: "The operation made no change".into(),
                    },
                    1,
                ))),
            });
            tracing::warn!(block = %focus, "{} changed nothing", describe(action));
        }
        std::thread::sleep(self.cfg.pace);
        Ok(())
    }
}

fn word_action_anchor(
    action: &WordProcessorAction,
    _focus: &str,
) -> qos_actions::InteractionAnchor {
    match action {
        WordProcessorAction::SpliceText {
            block,
            at,
            del,
            ins,
            ..
        } => qos_actions::InteractionAnchor::Text {
            region: "document".into(),
            ranges: vec![qos_actions::InteractionTextRange {
                block: block.clone(),
                start: u64::try_from(*at).unwrap_or(u64::MAX),
                end: u64::try_from(at.saturating_add((*del).max(ins.chars().count())))
                    .unwrap_or(u64::MAX),
            }],
        },
        WordProcessorAction::SetMark { spans, .. } => qos_actions::InteractionAnchor::Text {
            region: "document".into(),
            ranges: spans
                .iter()
                .map(|span| qos_actions::InteractionTextRange {
                    block: span.block.clone(),
                    start: u64::try_from(span.start).unwrap_or(u64::MAX),
                    end: u64::try_from(span.end).unwrap_or(u64::MAX),
                })
                .collect(),
        },
        WordProcessorAction::SetParagraphStyle { blocks, .. }
        | WordProcessorAction::ApplyNamedStyle { blocks, .. }
        | WordProcessorAction::SetBlockAttr { blocks, .. }
        | WordProcessorAction::RemoveBlocks { blocks } => qos_actions::InteractionAnchor::Objects {
            ids: blocks.clone(),
        },
        WordProcessorAction::InsertParagraph { after, .. }
        | WordProcessorAction::InsertTable { after, .. } => {
            qos_actions::InteractionAnchor::Objects {
                ids: vec![after.clone()],
            }
        }
        WordProcessorAction::InsertImage { block, .. }
        | WordProcessorAction::InsertPageBreak { block, .. } => {
            qos_actions::InteractionAnchor::Objects {
                ids: vec![block.clone()],
            }
        }
        WordProcessorAction::ReplaceBody { .. } => qos_actions::InteractionAnchor::Resource,
        WordProcessorAction::AcceptChanges { id } | WordProcessorAction::RejectChanges { id } => id
            .as_ref()
            .map_or(qos_actions::InteractionAnchor::Resource, |id| {
                qos_actions::InteractionAnchor::Objects {
                    ids: vec![id.clone()],
                }
            }),
        WordProcessorAction::ResolveComment { id } => qos_actions::InteractionAnchor::Objects {
            ids: vec![id.clone()],
        },
    }
}

fn describe(action: &WordProcessorAction) -> String {
    match action {
        WordProcessorAction::SpliceText { ins, del, .. } => {
            format!("splice text (+{} -{del})", ins.chars().count())
        }
        WordProcessorAction::InsertTable { rows, cols, .. } => {
            format!("insert table {rows}x{cols}")
        }
        WordProcessorAction::InsertParagraph { .. } => "insert paragraph".to_string(),
        WordProcessorAction::RemoveBlocks { blocks } => format!("remove {} block(s)", blocks.len()),
        WordProcessorAction::SetParagraphStyle { kind, .. } => format!("set style {kind}"),
        other => format!("{other:?}"),
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
            action_context: serde_json::json!({
                "purpose": "Review draft",
                "group": {"id": "task:review", "label": "Review draft", "kind": "task"}
            }),
        }
    }

    #[test]
    fn task_run_requires_broker_bound_task_context() {
        assert!(validate_task_run(&task_run()).is_ok());
        let mut mismatched = task_run();
        mismatched.action_context = serde_json::json!({
            "purpose": "Review draft",
            "group": {"id": "different", "label": "Review draft", "kind": "task"}
        });
        assert!(validate_task_run(&mismatched).is_err());
    }

    #[test]
    fn task_run_rejects_unknown_protocol_version_before_planning() {
        let mut invalid = task_run();
        invalid.version = 2;
        assert!(validate_task_run(&invalid).is_err());
    }

    #[test]
    fn a_content_step_uses_only_the_immediately_preceding_application_result() {
        let output = qos_actions::ActionOutput {
            value: Some(serde_json::json!({"display": "ignored"})),
            resources: vec![
                qos_actions::content_resource("abc123")
                    .expect("the producer returned a valid content hash"),
            ],
        };
        let previous = StepOutcome {
            output,
            product: Some(surface::Product::Content {
                hash: "legacy-derived-product".into(),
            }),
        };
        let planned = serde_json::Map::from_iter([(
            qos_actions::PROMPTED_PARAM.to_string(),
            serde_json::Value::String("model-invented".into()),
        )]);

        let bound = arguments_for_step(true, &planned, Some(&previous))
            .expect("one content result binds the next content input");
        assert_eq!(
            bound.get(qos_actions::PROMPTED_PARAM),
            Some(&serde_json::Value::String("abc123".into())),
            "the preceding app result must replace model input and its legacy product fallback"
        );
        assert!(
            arguments_for_step(true, &planned, None).is_err(),
            "a content action cannot reuse an older or invented result"
        );
    }

    #[test]
    fn a_content_step_refuses_ambiguous_or_wrong_kind_results() {
        let ambiguous = StepOutcome {
            output: qos_actions::ActionOutput {
                value: None,
                resources: vec!["qos.content:first".into(), "qos.content:second".into()],
            },
            product: None,
        };
        assert!(
            arguments_for_step(true, &serde_json::Map::new(), Some(&ambiguous)).is_err(),
            "choosing one of two products would be a model-visible guess"
        );

        let wrong_kind = StepOutcome {
            output: qos_actions::ActionOutput {
                value: None,
                resources: vec!["files.entry:/tmp/picture.png".into()],
            },
            product: None,
        };
        assert!(
            arguments_for_step(true, &serde_json::Map::new(), Some(&wrong_kind)).is_err(),
            "a generic resource address is not proof that content bytes exist"
        );
    }

    #[test]
    fn a_local_apps_declared_product_can_feed_the_next_content_step() {
        let previous = StepOutcome {
            output: qos_actions::ActionOutput::default(),
            product: Some(surface::Product::Content {
                hash: "rendered-photo".into(),
            }),
        };
        let bound = arguments_for_step(true, &serde_json::Map::new(), Some(&previous))
            .expect("the app, not the agent, declared the local document's product");
        assert_eq!(
            bound.get(qos_actions::PROMPTED_PARAM),
            Some(&serde_json::Value::String("rendered-photo".into()))
        );
    }

    #[test]
    fn a_zero_settle_never_waits() {
        let now = Instant::now();
        assert!(Session::settled(None, "Scribe: table", now, Duration::ZERO));
        assert!(Session::settled(
            Some(&("Scribe: table".to_string(), now)),
            "Scribe: table",
            now,
            Duration::ZERO
        ));
    }

    #[test]
    fn the_first_sight_of_a_line_is_never_settled() {
        let now = Instant::now();
        assert!(!Session::settled(
            None,
            "Scribe: table",
            now,
            Duration::from_millis(500)
        ));
    }

    #[test]
    fn a_line_that_changed_since_it_was_last_seen_is_not_settled() {
        let now = Instant::now();
        let watched = ("Scribe: tab".to_string(), now);
        assert!(!Session::settled(
            Some(&watched),
            "Scribe: table",
            now,
            Duration::from_millis(500)
        ));
    }

    #[test]
    fn unchanged_text_settles_once_the_wait_has_elapsed_and_not_before() {
        let settle = Duration::from_millis(500);
        let seen = Instant::now();
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
