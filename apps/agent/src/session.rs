//! One agent session: join the workspace and answer the conversations it is
//! asked in.

use std::path::PathBuf;
use ui::time::{Duration, Instant};

use anyhow::Result;
use ui::collaboration::{CollabEvent, CollabRequest, PresencePayload, RouteSpec};
use ui::permissions::PermissionContext;
use ui::presence::PresenceKey;
use uuid::Uuid;

use crate::model::Chooser;
use crate::providers::agent_selection;
use crate::surface;
use crate::wire::Link;

mod agent_loop;
/// What a failed turn says, split into the person's sentence and the run's resource.
pub(crate) use agent_loop::Failure;

mod confirmation;
mod conversation;
mod planning;
mod selection_prompt;
mod steps;
mod summary;

/// How long a task waits for the broker's pushed action catalogue.
const TASK_CATALOGUE_WAIT: Duration = Duration::from_secs(2);
/// A turn the broker handed this agent: its answer, invoked with what the person asked.
#[derive(Clone, Debug)]
pub struct Asked {
    /// The handle the broker delivered the answer under: what its updates name.
    pub id: u64,
    /// The answer's own invocation.
    pub invocation: tilo_actions::InvocationId,
    /// What the person asked, and what the agent may spend on it.
    pub asked: runtime_collab::TurnRequest,
    /// The context the host invoked the answer with: whom the agent acts for.
    pub context: Option<tilo_actions::ActionContext>,
}

impl std::ops::Deref for Asked {
    type Target = runtime_collab::TurnRequest;

    fn deref(&self) -> &Self::Target {
        &self.asked
    }
}

impl Asked {
    /// The person's words.
    #[must_use]
    pub fn instruction(&self) -> &str {
        &self.asked.instruction
    }
}

/// How a turn stands, as the agent tells its conversation and the broker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TurnUpdate {
    Accepted,
    Progress {
        message: String,
    },
    Succeeded {
        message: String,
    },
    Failed {
        message: String,
    },
    Cancelled {
        message: String,
    },
    /// What the turn spent by now, in millionths of the currency, as the model gateway charged
    /// it: the turn's total, not the last call's.
    Spent {
        cost_micros: u64,
    },
}

/// What a turn's answer reports beside its progress: what it spent by now.
fn spent_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["spent"],
        "properties": { "spent": { "type": "integer", "minimum": 0 } }
    })
}

/// How the agent answers a turn, as it publishes it: the host alone invokes it, with what the
/// person asked, and follows it to the reply; what the turn spent comes back as its items.
#[must_use]
pub fn answer_descriptor() -> tilo_actions::ActionDescriptor {
    let mut contract = tilo_actions::ActionContract {
        kind: tilo_actions::ActionKind::Read,
        execution: tilo_actions::ExecutionMode::Streaming {
            cancellable: true,
            item_schema: spent_schema(),
        },
        result: tilo_actions::ResultSpec::Job,
        ..tilo_actions::ActionContract::default()
    };
    contract.set_audience(tilo_actions::Audience::Agents);
    tilo_actions::ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(
            runtime_collab::TURN_RESOURCE,
            runtime_collab::ANSWER,
        ),
        resource_kind: "agent.turn".into(),
        resource_label: "Turn".into(),
        action_label: "Answer".into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        locus: tilo_actions::ExecutionLocus::ClientLocal,
        kind: tilo_actions::OperationKind::Effect,
        contract,
        input: tilo_actions::InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

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
    /// What the action did, as its app declares it ("showed on the canvas"):
    /// how the reply's step line says it, beside its label.
    did: Option<String>,
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

/// A step of the turn the host accepted, followed to its outcome alone
/// ([`Session::invoke_task_action`]) or beside others started in the same
/// turn. Each keeps its own invocation id and broker job: following several
/// at once shares only the waiting.
struct Following {
    action: TaskAction,
    invocation: tilo_actions::InvocationId,
    /// How the step is observed: by asking the host for it.
    reask: steps::Reask,
    /// The latest state the host reported.
    update: tilo_actions::InvocationUpdate,
    /// When the step gives up, where it has a time: a step that asks the
    /// person first has none of its own but the run's budget — the person
    /// decides in their own time, and nothing else cuts it short.
    deadline: Option<Instant>,
    /// Its outcome, once settled.
    outcome: Option<Result<StepOutcome, TaskActionStop>>,
}

impl Following {
    fn into_outcome(self) -> Result<StepOutcome, TaskActionStop> {
        self.outcome.unwrap_or_else(|| {
            Err(TaskActionStop::Failed(crate::i18n::t(
                "agent-fail-no-terminal-state",
            )))
        })
    }
}

fn task_action_from_offer(
    offer: &surface::Offer,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<TaskAction, String> {
    let peer_id = match (offer.broker_peer_id, offer.broker_provider.as_ref()) {
        (Some(peer), _) => peer,
        (None, Some(_)) => 0,
        (None, None) => return Err(crate::i18n::t("agent-fail-local-only")),
    };
    if !offer.wants_content() {
        tilo_actions::ActionArgs::bind_json(&offer.descriptor.params, &arguments).map_err(
            |error| {
                crate::i18n::t_args(
                    "agent-fail-arguments",
                    &[("error", error.to_string().into())],
                )
            },
        )?;
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
        did: offer.descriptor.did.clone(),
        carries: offer.content_param(),
    })
}

#[derive(Clone, Debug)]
pub struct Config {
    /// Stable workspace-admitted identity, independent of the display name.
    pub agent_id: String,
    /// Explicit typed collaboration endpoint for headless diagnostics outside
    /// the launcher. Normal sessions leave this `None` and inherit the fd.
    pub endpoint: Option<PathBuf>,
    /// The app whose document this is — its manifest package id, which is
    /// what a launched copy of the app announces.
    pub app_id: String,
    /// The workspace the document lives in.
    pub cluster: Uuid,
    /// The resource the app announced for the document (`file:<path>`), or
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
    /// [`crate::surface::Catalogue::offers`].
    pub permissions: PermissionContext,
    /// The agent's content store, created once when it starts: content an
    /// operation produced is read from here by the operation consuming it.
    pub content: ui::content::ContentStore,
    /// The model the desktop chose for this agent, when it launched it with
    /// one; otherwise the Settings default this process can read, if any.
    pub provider: Option<crate::providers::ProviderSelection>,
    /// The certificate this connection is admitted with, when it is given one
    /// rather than inheriting the launcher's. A harness that runs an agent
    /// against a real broker over an explicit endpoint presents the same
    /// certificate a person's app does, so the agent is admitted as the agent
    /// the workspace keeps — never as an anonymous diagnostic peer.
    pub identity: Option<ui::collaboration::CertifiedEndpointIdentity>,
}

pub struct Session {
    cfg: Config,
    link: Link,
    /// Decides which exported action answers an instruction.
    chooser: Chooser,
    /// How many steps each request (by its digest) started in the turn so far.
    children: std::collections::HashMap<[u8; 32], u32>,
    /// The last sequence number the turn's answer reported under.
    reported: u64,
    /// The broker's pushed, permission-filtered live action directory the
    /// task loop surveys.
    broker_catalogue: ui::collaboration::ActionCatalogue,
    next_broker_request: u64,
    /// Bumped whenever the broker republishes its catalogue.
    broker_revision: u64,
    /// The task catalogue survey for [`Self::broker_revision`], shared by
    /// every reader until the broker republishes.
    catalogue_cache: Option<(u64, std::sync::Arc<surface::Catalogue>)>,
    /// What each app whose resources the session serves declares, compacted for the first turn,
    /// by the alias programs name it with: read once through the API's `describe` and kept, as
    /// an app's declarations change only when it is updated.
    declarations: std::collections::BTreeMap<String, String>,
    /// The model turn in flight while a task loop runs.
    turn: Option<agent_loop::ActiveTurn>,
    /// The turn the run in progress answers, as its reply is being told.
    record: Option<conversation::Record>,
    /// The run's working resource of the failure that ended its turn, kept for
    /// whoever debugs it later: the transport's error, the partial answer, the
    /// provider's exposed reasoning. The reply's own words are the failure's
    /// sentence, never this.
    failure_detail: Option<String>,
    /// What the run in progress may spend today, as its keeper counted it.
    spending: Option<agent_loop::Spending>,
    /// The agent's own web tools, with the client and the engines' backoff
    /// they keep from one call to the next; set up on the first call that
    /// needs them.
    web: Option<crate::tools::web::Web>,
    /// Whom this session asks a single question outside a turn's own exchange with its model:
    /// what a conversation's summary is kept with ([`summary`]).
    single_shot: std::sync::Arc<dyn crate::transform::TransformModel>,
}

/// Ask the broker to select one current holder of this exact route. Safe to
/// repeat while no holder is available; unlike presence fan-out, one request
/// produces at most one full snapshot.
fn ask_for_document(link: &mut Link) -> Result<()> {
    link.send(&CollabRequest::RequestDocumentSnapshot)
}

impl Session {
    /// Connect, announce the route, and ask the workspace for the document.
    pub fn connect(cfg: Config) -> Result<Self> {
        let managed = managed_context(&cfg)?;
        let mut link = match &cfg.endpoint {
            Some(path) => Link::connect_endpoint(
                path,
                &cfg.app_id,
                cfg.cluster,
                cfg.record.clone(),
                Some(cfg.agent_id.clone()),
                cfg.identity.clone(),
            )?,
            None => Link::connect(
                &cfg.app_id,
                cfg.cluster,
                cfg.record.clone(),
                Some(cfg.agent_id.clone()),
                None,
            )?,
        };
        // Discovery is workspace-wide, but writes never are. This subscription
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
            workspace = %cfg.cluster,
            record = ?cfg.record,
            name = %cfg.name,
            "joined the workspace; waiting for the document"
        );
        let chooser = match managed {
            Some(selection) => {
                let chooser = Chooser::with_provider(true, cfg.model_timeout, selection);
                anyhow::ensure!(chooser.is_live(), "managed model gateway unavailable");
                chooser
            }
            None => cfg
                .provider
                .clone()
                .or_else(|| agent_selection(&cfg.agent_id))
                .map_or_else(
                    || Chooser::new(false, cfg.model_timeout),
                    |selection| Chooser::with_provider(cfg.model, cfg.model_timeout, selection),
                ),
        };
        let single_shot = std::sync::Arc::new(crate::transform::GatewayModel {
            launched: chooser.selection().cloned(),
        });
        Ok(Self {
            cfg,
            link,
            chooser,
            children: std::collections::HashMap::new(),
            reported: 0,
            broker_catalogue: ui::collaboration::ActionCatalogue::default(),
            next_broker_request: 1,
            broker_revision: 0,
            catalogue_cache: None,
            declarations: std::collections::BTreeMap::new(),
            turn: None,
            record: None,
            failure_detail: None,
            spending: None,
            web: None,
            single_shot,
        })
    }

    /// The agent's web tools, ready to run a search or read a page: their
    /// client, runtime and engines' backoff are made once and kept.
    fn web(&mut self) -> &mut crate::tools::web::Web {
        let content = self.cfg.content.clone();
        self.web
            .get_or_insert_with(|| crate::tools::web::Web::new(content))
    }

    /// Answer the turns the broker delivers: an unattended run may invoke
    /// only actions that the authenticated broker currently publishes to this
    /// agent.
    pub fn run(&mut self) -> Result<()> {
        let mut transforms = crate::transform::TransformService::new(
            std::sync::Arc::new(crate::transform::GatewayModel {
                launched: self.chooser.selection().cloned(),
            }),
            self.cfg.content.clone(),
        );
        // The transforms, and how the agent answers a turn, are published under the agent's own
        // route; a connection announcing another app may not publish for it.
        if self.cfg.app_id == tilo_automation::AGENT_APP_ID {
            self.link
                .send(&crate::transform::publication(&[answer_descriptor()]))?;
            tracing::info!(
                resource = crate::transform::RESOURCE_ID,
                "published the AI transforms and the turns' answer"
            );
        }
        tracing::info!(agent = %self.cfg.name, "waiting for the conversations' turns");
        loop {
            match self.link.recv(Duration::from_millis(100))? {
                Some(CollabEvent::ActionCatalogue { change }) => {
                    self.apply_broker_catalogue(&change);
                }
                Some(CollabEvent::Invocation {
                    id,
                    invocation,
                    resource,
                    action,
                    argument,
                    context,
                    ..
                }) if resource == runtime_collab::TURN_RESOURCE
                    && action == runtime_collab::ANSWER =>
                {
                    self.answer(
                        id,
                        invocation,
                        argument.as_deref(),
                        context.map(|context| *context),
                    )?;
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
                Some(_) => {}
                // Nothing will arrive once the broker closed the session (a
                // refused admission, a revoked device, an ended task): the
                // process ends with the reason instead of waiting on nothing.
                None => {
                    if let Some(reason) = self.link.closed() {
                        anyhow::bail!("the broker closed the agent's session: {reason}");
                    }
                }
            }
            for (id, update) in transforms.finished() {
                self.link
                    .send(&CollabRequest::UpdateInvocation { id, update })?;
            }
        }
    }

    /// Answer the turn the broker handed as `id`: its answer's invocation, with what the person
    /// asked as `argument`. A turn that stopped before its reply was told is reported failed.
    fn answer(
        &mut self,
        id: u64,
        invocation: tilo_actions::InvocationId,
        argument: Option<&str>,
        context: Option<tilo_actions::ActionContext>,
    ) -> Result<()> {
        let Some(Ok(asked)) = argument.map(serde_json::from_str::<runtime_collab::TurnRequest>)
        else {
            tracing::warn!(id, "refused a turn whose request does not read");
            return self.link.send(&CollabRequest::UpdateInvocation {
                id,
                update: tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "turn_unreadable".into(),
                        message: "The turn's request does not read".into(),
                        details: None,
                        retryable: false,
                    },
                },
            });
        };
        let asked = Asked {
            id,
            invocation,
            asked,
            context,
        };
        if let Err(error) = self.run_task(&asked) {
            tracing::warn!(id, %error, "the turn stopped before its reply was told");
            if let Err(report_error) = self.fail_task_said(&asked, &error.to_string()) {
                tracing::warn!(%report_error, "the turn's failure was not reported");
            }
        }
        self.link.forget_cancelled(id);
        Ok(())
    }

    /// Plan with `chooser` from now on, in place of the one this session
    /// connected with: how a harness scripts the model a turn is planned
    /// with, while every other part of the run stays the production one.
    pub fn set_chooser(&mut self, chooser: Chooser) {
        self.chooser = chooser;
    }

    /// Ask single questions of `model` from now on, in place of the gateway this session
    /// connected with: how a harness scripts them, and how a host answers them with the model of
    /// the person the agent works for.
    pub fn set_single_shot(&mut self, model: std::sync::Arc<dyn crate::transform::TransformModel>) {
        self.single_shot = model;
    }

    /// The chooser this session plans with now.
    #[must_use]
    pub fn chooser(&self) -> &Chooser {
        &self.chooser
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one task run keeps planning, per-step reporting and the terminal update in a single auditable sequence"
    )]
    fn run_task(&mut self, run: &Asked) -> Result<()> {
        // What this run says is the person's language as it stands now: they
        // may have changed it since the app started.
        crate::i18n::follow();
        self.children.clear();
        self.reported = 0;
        let cause = match conversation::validate(run) {
            Ok(cause) => cause,
            Err(message) => {
                tracing::warn!(id = run.id, %message, "refused a turn that answers no message");
                self.report_task(run, TurnUpdate::Failed { message })?;
                return Ok(());
            }
        };
        self.record = None;
        let plan = self.planner();
        self.start_turn(cause);
        // The reply that answers the turn is recorded through the
        // conversation's own action, so the actions this workspace publishes
        // are read before the first record: the person sees the reply the
        // moment the run is taken.
        self.wait_for_task_catalogue();
        self.report_task(run, TurnUpdate::Accepted)?;
        self.report_task(
            run,
            TurnUpdate::Progress {
                message: crate::i18n::t("agent-session-reading-actions"),
            },
        )?;
        // Which planner decides this run's steps, said before it decides:
        // the model that plans it, or that its steps are matched to the
        // published actions and why no model plans instead.
        self.report_task(
            run,
            TurnUpdate::Progress {
                message: plan.announcing(),
            },
        )?;
        // A model task is a discover → invoke → observe loop over the live
        // catalogue. A managed agent has no other planner; one without a
        // model (`--no-model`) falls back to the rules-only survey below.
        if plan.is_model() {
            let end = self.run_task_loop(run)?;
            return match end {
                agent_loop::LoopEnd::Finished(finish, completed) => {
                    if !finish.done {
                        return self.fail_task(
                            run,
                            &agent_loop::Failure::said(
                                finish
                                    .reply
                                    .as_deref()
                                    .unwrap_or(&crate::i18n::t("agent-session-needs-decision")),
                            ),
                        );
                    }
                    // The model's own reply is the run's result, whole: an
                    // answer to a question is what the issue asked for. A run
                    // that ends with nothing said is not reported as a success
                    // with a line nobody wrote: the person reads that the
                    // agent finished without answering.
                    self.succeed_turn(run, finish.reply, planning::finished(&plan, completed))
                }
                agent_loop::LoopEnd::Failed(failure) => self.fail_task(run, &failure),
                agent_loop::LoopEnd::Cancelled(message) => self.cancel_task(run, &message),
            };
        }
        if self.task_cancelled(run) {
            return self.cancel_task(
                run,
                &crate::i18n::t("agent-session-cancelled-before-planning"),
            );
        }
        let (actions, reply) = match self.plan_task(run, &plan) {
            Ok((actions, reply)) if !actions.is_empty() => (actions, reply),
            Ok(_) => {
                return self.fail_task_said(
                    run,
                    &planning::unmatched(&plan, &crate::i18n::t("agent-plan-no-action")),
                );
            }
            Err(message) => return self.fail_task_said(run, &message),
        };
        // What the next content-consuming action is built from.
        let mut previous: Option<StepOutcome> = None;
        for (index, action) in actions.iter().enumerate() {
            if self.task_cancelled(run) {
                return self.cancel_task(
                    run,
                    &crate::i18n::t("agent-session-cancelled-before-action"),
                );
            }
            let arguments = match arguments_for_step(
                action.carries.as_deref(),
                &action.arguments,
                previous.as_ref(),
            ) {
                Ok(arguments) => arguments,
                Err(message) => return self.fail_task_said(run, &message),
            };
            self.report_task(
                run,
                TurnUpdate::Progress {
                    message: crate::i18n::t_args(
                        "agent-session-step",
                        &[
                            ("index", (index + 1).into()),
                            ("total", actions.len().into()),
                            ("action", action.label.as_str().into()),
                        ],
                    ),
                },
            )?;
            let settled = self.run_task_step(run, index, action, &arguments)?;
            self.resource_action(run, action, settled.as_ref())?;
            match settled {
                Ok(outcome) => {
                    self.chooser
                        .mark(tilo_model_client::TraceEvent::ActionSettled, index);
                    previous = Some(outcome);
                }
                Err(TaskActionStop::Cancelled(_)) => {
                    return self
                        .cancel_task(run, &crate::i18n::t("agent-session-cancelled-in-action"));
                }
                Err(TaskActionStop::Failed(message)) => return self.fail_task_said(run, &message),
            }
        }
        self.succeed_turn(run, reply, planning::finished(&plan, actions.len()))
    }

    /// Invoke one planned action through the broker's invocation path.
    fn run_task_step(
        &mut self,
        run: &Asked,
        index: usize,
        action: &TaskAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Result<StepOutcome, TaskActionStop>> {
        if self.task_cancelled(run) {
            return Ok(Err(TaskActionStop::Cancelled(Some(
                "Cancelled before the next step".into(),
            ))));
        }
        self.chooser
            .mark(tilo_model_client::TraceEvent::ActionSubmitted, index);
        self.invoke_task_action(run, action, arguments)
    }

    /// Tell the conversation and the broker how `run` stands: the reply to its message shows it,
    /// and the turn's answer reports it.
    fn report_task(&mut self, run: &Asked, update: TurnUpdate) -> Result<()> {
        self.show_turn(run, &update)?;
        // How the turn ended, with what the person reads, once: the resource a run's
        // reader (and the live eval, `dist/dev/agent-eval.py`) finds its end by.
        let ended = match &update {
            TurnUpdate::Succeeded { message } => Some(("succeeded", message)),
            TurnUpdate::Failed { message } => Some(("failed", message)),
            TurnUpdate::Cancelled { message } => Some(("cancelled", message)),
            _ => None,
        };
        if let Some((state, message)) = ended {
            let steps = self.record.as_ref().map_or(0, |record| record.steps.len());
            tracing::info!(id = run.id, state, steps, reply = ?message, "the turn ended");
        }
        let invocation = run.invocation;
        let update = match update {
            // The broker took the turn when it handed it over.
            TurnUpdate::Accepted => return Ok(()),
            TurnUpdate::Progress { message } => {
                self.reported = self.reported.saturating_add(1);
                tilo_actions::InvocationUpdate::Progress {
                    invocation,
                    sequence: self.reported,
                    phase: "working".into(),
                    completed: None,
                    total: None,
                    message: Some(message),
                }
            }
            TurnUpdate::Spent { cost_micros } => {
                self.reported = self.reported.saturating_add(1);
                tilo_actions::InvocationUpdate::Item {
                    invocation,
                    sequence: self.reported,
                    value: serde_json::json!({ "spent": cost_micros }),
                }
            }
            TurnUpdate::Succeeded { message } => tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({ "reply": message })),
                    resources: Vec::new(),
                },
            },
            TurnUpdate::Failed { message } => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "turn_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
            TurnUpdate::Cancelled { message } => tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some(message),
            },
        };
        self.link
            .send(&CollabRequest::UpdateInvocation { id: run.id, update })
    }

    /// Whether the person ended `run`: the broker asked to stop its answer.
    fn task_cancelled(&mut self, run: &Asked) -> bool {
        self.link.cancelled(run.id)
    }

    /// Record a failed turn on the reply to the run's message, and tell the
    /// host it failed. The reply's words are the failure's sentence — what a
    /// person reads — and the run's working resource goes beside it in the work,
    /// for whoever debugs it later.
    fn fail_task(&mut self, run: &Asked, failure: &agent_loop::Failure) -> Result<()> {
        self.failure_detail = Some(failure.detail().to_owned());
        self.report_task(
            run,
            TurnUpdate::Failed {
                message: failure.message().to_owned(),
            },
        )
    }

    /// [`Self::fail_task`] with the run's own words as both the sentence and
    /// the record: a stop the loop decided, which names nothing a provider
    /// said.
    fn fail_task_said(&mut self, run: &Asked, message: &str) -> Result<()> {
        self.fail_task(run, &agent_loop::Failure::said(message))
    }

    fn cancel_task(&mut self, run: &Asked, message: &str) -> Result<()> {
        self.report_task(
            run,
            TurnUpdate::Cancelled {
                message: message.to_string(),
            },
        )
    }

    fn wait_for_task_catalogue(&mut self) {
        let wait = Self::task_wait_deadline(TASK_CATALOGUE_WAIT)
            .saturating_duration_since(ui::time::now());
        let since = self.broker_revision;
        self.await_catalogue(wait, |session| {
            session.broker_revision != since || !session.broker_catalogue.surfaces().is_empty()
        });
    }

    fn task_wait_deadline(maximum: Duration) -> Instant {
        let now = ui::time::now();
        now.checked_add(maximum).unwrap_or(now)
    }

    /// The rules-only planner: one surveyed action matched by the words of
    /// the title. Model tasks run [`Self::run_task_loop`] instead.
    fn plan_task(
        &mut self,
        run: &Asked,
        plan: &planning::Planner,
    ) -> Result<(Vec<TaskAction>, Option<String>), String> {
        let catalogue = self.task_catalogue();
        let (offer, arguments) = surface::from_verbs(run.instruction(), &catalogue.offers)
            .map_err(|no_match| planning::unmatched(plan, &no_match.explanation()))?;
        task_action_from_offer(offer, arguments).map(|action| (vec![action], None))
    }

    /// How this turn will decide its steps, as its note states it.
    fn planner(&self) -> planning::Planner {
        match self.chooser.planned_by() {
            Some((provider, model)) => planning::Planner::Model {
                provider: provider.to_owned(),
                model: model.to_owned(),
            },
            // A model task whose model cannot be reached: the run still
            // happens, matched to the published actions, and its note says
            // which model is missing and where to choose one.
            None => planning::Planner::Words,
        }
    }

    fn invoke_task_action(
        &mut self,
        run: &Asked,
        action: &TaskAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Result<StepOutcome, TaskActionStop>> {
        let following = match self.start_task_invocation(run, action, arguments)? {
            Ok(following) => following,
            Err(stop) => return Ok(Err(stop)),
        };
        let mut followed = [following];
        self.follow_native(run, &mut followed)?;
        let [following] = followed;
        Ok(following.into_outcome())
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_broker_request;
        self.next_broker_request = self.next_broker_request.saturating_add(1);
        id
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
                    | "workspace"
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
            && text("workspace")? == cfg.cluster.to_string()
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

    #[test]
    fn a_steps_arguments_bind_exactly_one_preceding_content_result_and_key_its_dispatch() {
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
