//! One agent session: join the room and carry out the runs of its issues.

use std::path::PathBuf;
use ui::time::{Duration, Instant};

use anyhow::Result;
use ui::collaboration::{
    CertifiedEndpointIdentity, CollabEvent, CollabRequest, PresencePayload, RouteSpec,
};
use ui::permissions::PermissionContext;
use ui::presence::PresenceKey;
use uuid::Uuid;

use crate::model::Chooser;
use crate::providers::agent_selection;
use crate::surface;
use crate::wire::Link;

mod agent_loop;
mod confirmation;
mod native_task;
mod outstanding;
mod planning;
mod selection_prompt;
mod tracker;

/// How long a task waits for the broker's pushed action catalogue.
const TASK_CATALOGUE_WAIT: Duration = Duration::from_secs(2);
const TASK_ACTION_WAIT: Duration = Duration::from_secs(30);
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

/// A child of the run the host accepted, followed to its outcome alone
/// ([`Session::invoke_task_action`]) or beside others started in the same
/// turn. Each keeps its own invocation id and broker job: following several
/// at once shares only the waiting.
struct Following {
    action: TaskAction,
    /// The arguments it was started with: a native child is re-asked with
    /// the identical request.
    arguments: serde_json::Map<String, serde_json::Value>,
    invocation: tilo_actions::InvocationId,
    /// How the child is observed: by re-asking the host for it.
    reask: native_task::Reask,
    /// The latest state the host reported.
    update: tilo_actions::InvocationUpdate,
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
        || run.lease.len() > 128
        || runtime_collab::native_task_revision(&run.lease).is_none()
        || tracker::issue_of(run).is_none()
    {
        return Err("The task envelope has an invalid issue, title, or lease".into());
    }
    if let Some(budget) = run.execution_budget {
        budget.validate().map_err(str::to_owned)?;
    }
    let context = serde_json::from_value::<tilo_actions::ActionContext>(run.action_context.clone())
        .map_err(|_| "The task has an invalid action context".to_string())?;
    let valid_group = context.group.as_ref().is_some_and(|group| {
        group.id == run.group() && group.label == run.title && group.kind == "task"
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
    /// [`crate::surface::Catalogue::offers`].
    pub permissions: PermissionContext,
    /// The agent's content store, created once when it starts: content an
    /// operation produced is read from here by the operation consuming it.
    pub content: ui::content::ContentStore,
    /// Applications started beside the agent whose actions its first run
    /// waits for (`Session::await_companions`).
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
    /// What the task in progress asked its host without waiting in line.
    outstanding: outstanding::Outstanding,
    /// What the model spent on the run in progress, as its provider counted
    /// it: reported on the run's thread when the run ends.
    spend: planning::Spend,
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
            None => cfg
                .provider
                .clone()
                .or_else(|| agent_selection(&cfg.agent_id))
                .map_or_else(
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
            awaiting_companions: cfg_companions,
            broker_revision: 0,
            catalogue_cache: None,
            turn: None,
            outstanding: outstanding::Outstanding::default(),
            spend: planning::Spend::default(),
        })
    }

    /// Run the issues the broker delivers: an unattended run may invoke
    /// only actions that the authenticated broker currently publishes to this
    /// agent.
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
                Some(CollabEvent::TaskRun(run)) => {
                    if let Err(error) = self.run_task(&run) {
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

    /// Run `run`, planning with the model chosen for it when it names one.
    fn run_task(&mut self, run: &runtime_collab::TaskRunEnvelope) -> Result<()> {
        let chosen = run.model.as_ref().and_then(|model| self.run_chooser(model));
        let own = chosen.map(|chooser| std::mem::replace(&mut self.chooser, chooser));
        let result = self.run_task_steps(run);
        if let Some(own) = own {
            self.chooser = own;
        }
        result
    }

    /// The chooser that plans with `model` for one run: the provider's
    /// profile as Settings keeps it, or for a managed agent its gateway's
    /// selection with that model. `None`, and the agent's own model, when
    /// the model is not offered here.
    fn run_chooser(&self, model: &runtime_collab::TaskRunModel) -> Option<Chooser> {
        let config = agent_provider_config::ProviderConfig::load();
        let found = config
            .profiles
            .iter()
            .find(|profile| profile.id == model.profile && profile.enabled)
            .and_then(|profile| {
                profile
                    .models
                    .iter()
                    .find(|offered| offered.id == model.model)
                    .map(|offered| (profile, offered.clone()))
            });
        let Some((profile, descriptor)) = found else {
            tracing::warn!(profile = %model.profile, model = %model.model, "the run's model is not offered here; planning with the agent's own");
            return None;
        };
        let selection = if self.managed {
            let mut selection = self.chooser.selection()?.clone();
            if selection.profile_id != profile.id {
                tracing::warn!(profile = %model.profile, "a managed agent plans only through its own provider");
                return None;
            }
            selection.model = descriptor;
            selection
        } else {
            crate::providers::ProviderSelection::from_profile(profile, descriptor)
        };
        Some(Chooser::with_provider(
            self.managed || self.cfg.model,
            self.cfg.model_timeout,
            selection,
        ))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one task run keeps planning, per-step reporting and the terminal update in a single auditable sequence"
    )]
    fn run_task_steps(&mut self, run: &runtime_collab::TaskRunEnvelope) -> Result<()> {
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
        self.spend = planning::Spend::default();
        self.report_task(run, runtime_collab::TaskRunUpdate::Accepted)?;
        // The cancellation poll is not waited for here: its answer, with the
        // run's messages, is read before the first turn (or the first
        // planned action), which it reaches the host ahead of.
        self.report_task(
            run,
            runtime_collab::TaskRunUpdate::Progress {
                message: "Reading the actions this room's apps publish".into(),
                presentation: None,
            },
        )?;
        self.poll_cancellation(run)?;
        self.wait_for_task_catalogue()?;
        // Which planner decides this run's steps, said before it decides:
        // the model that plans it, or that its steps are matched to the
        // published actions and why no model plans instead.
        let plan = self.planner(run);
        self.report_task(
            run,
            runtime_collab::TaskRunUpdate::Progress {
                message: plan.announcing(),
                presentation: None,
            },
        )?;
        // A model task is a discover → invoke → observe loop over the live
        // catalogue. A managed agent has no other planner; one without a
        // model (`--no-model`) falls back to the rules-only survey below.
        if plan.is_model() {
            let end = self.run_task_loop(run)?;
            self.report_spend(run, &plan)?;
            return match end {
                agent_loop::LoopEnd::Finished(finish, completed) => {
                    if !finish.done {
                        return self.fail_task(
                            run,
                            finish
                                .reply
                                .as_deref()
                                .unwrap_or("Needs your decision before continuing"),
                        );
                    }
                    // The model's own reply is the run's result, whole: an
                    // answer to a question is what the issue asked for.
                    let message = finish.reply.unwrap_or_else(|| {
                        format!("Completed {completed} action(s) the broker approved")
                    });
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
        let (actions, reply) = match self.plan_task(run, &plan) {
            Ok((actions, reply)) if !actions.is_empty() => (actions, reply),
            Ok(_) => {
                return self.fail_task(
                    run,
                    &planning::unmatched(
                        &plan,
                        "No published action carries the words of the request.",
                    ),
                );
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
                        .mark(tilo_model_client::TraceEvent::ActionSettled, index);
                    previous = Some(outcome);
                }
                Err(TaskActionStop::Cancelled(_)) => {
                    return self.cancel_task(run, "Cancelled while an action was running");
                }
                Err(TaskActionStop::Failed(message)) => return self.fail_task(run, &message),
            }
        }
        let message = reply.unwrap_or_else(|| planning::finished(&plan, actions.len()));
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
            .mark(tilo_model_client::TraceEvent::ActionSubmitted, index);
        let settled = self.invoke_task_action(run, action, arguments)?;
        self.step_settled();
        Ok(settled)
    }

    fn report_task(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        update: runtime_collab::TaskRunUpdate,
    ) -> Result<()> {
        self.report_to_tracker(run, &update)?;
        let request_id = self.next_request_id();
        self.link.send(&CollabRequest::ReportTaskRun {
            request_id: Some(request_id),
            run_id: run.run_id,
            lease: self.task_lease(run),
            update,
        })?;
        self.wait_for_native_report(request_id)
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
        plan: &planning::Planner,
    ) -> Result<(Vec<TaskAction>, Option<String>), String> {
        let catalogue = self.task_catalogue();
        let (offer, arguments) = surface::from_verbs(run.instruction(), &catalogue.offers)
            .map_err(|no_match| planning::unmatched(plan, &no_match.explanation()))?;
        task_action_from_offer(offer, arguments).map(|action| (vec![action], None))
    }

    /// How this run will decide its steps, as the run's note states it.
    fn planner(&self, run: &runtime_collab::TaskRunEnvelope) -> planning::Planner {
        if !matches!(run.planning, runtime_collab::TaskRunPlanning::Model) {
            return planning::Planner::Words {
                reason: planning::Words::Asked,
            };
        }
        match self.chooser.planned_by() {
            Some((provider, model)) => planning::Planner::Model {
                provider: provider.to_owned(),
                model: model.to_owned(),
            },
            // A model task whose model cannot be reached: the run still
            // happens, matched to the published actions, and its note says
            // which model is missing and where to choose one.
            None => planning::Planner::Words {
                reason: planning::Words::NoModel,
            },
        }
    }

    /// Record what the model spent on this run, on the run's own thread: the
    /// model, its turns and its tokens, or that it counted none.
    fn report_spend(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        plan: &planning::Planner,
    ) -> Result<()> {
        let report = self.spend.report(plan);
        if report.is_empty() {
            return Ok(());
        }
        self.report_task(
            run,
            runtime_collab::TaskRunUpdate::Progress {
                message: report,
                presentation: None,
            },
        )
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
        self.follow_native(run, &mut followed)?;
        let [following] = followed;
        Ok(following.into_outcome())
    }

    /// Ask the host to start one child of the run: authorized, journaled
    /// and accepted by the broker on its own, exactly as when it is followed
    /// alone. The host answers with the accepted state at once; the action
    /// then runs in its application while this agent starts the next one or
    /// follows it ([`Self::follow_native`]).
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
        Ok(Ok(Following {
            action: action.clone(),
            arguments: arguments.clone(),
            invocation,
            reask: native_task::Reask::default(),
            update,
            deadline: self.task_wait_deadline(TASK_ACTION_WAIT),
            outcome: None,
        }))
    }

    fn wait_for_task_action_result(
        &mut self,
        request_id: u64,
    ) -> Result<Result<(u64, tilo_actions::InvocationUpdate), String>> {
        let deadline = self.task_wait_deadline(Duration::from_secs(10));
        self.task_action_result_until(request_id, Some(deadline))
    }

    /// The broker's answer to task action request `request_id`, given by
    /// `deadline` if there is one.
    fn task_action_result_until(
        &mut self,
        request_id: u64,
        deadline: Option<Instant>,
    ) -> Result<Result<(u64, tilo_actions::InvocationUpdate), String>> {
        const TURN_PUMP: Duration = Duration::from_millis(100);
        let mut deferred = Vec::new();
        loop {
            // Keep a streaming model turn moving while the step settles.
            self.pump_turn();
            let now = ui::time::now();
            if deadline.is_some_and(|deadline| now >= deadline) {
                self.link.defer(deferred);
                return Ok(Err(
                    "the broker did not answer the task action request".into()
                ));
            }
            let wait = deadline.map_or(TURN_PUMP, |deadline| (deadline - now).min(TURN_PUMP));
            match self.link.recv(wait)? {
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
            model: None,
            version: 1,
            task_id: "tasks.tracker/issues/review".into(),
            title: "Review draft".into(),
            instruction: None,
            subject: None,
            run_id: 7,
            lease: "native:00000000000000000000000000000000:0".into(),
            planning: runtime_collab::TaskRunPlanning::RulesOnly,
            presentation_version: None,
            execution_budget: None,
            action_context: serde_json::json!({
                "purpose": "Review draft",
                "group": {
                    "id": runtime_collab::task_run_group("tasks.tracker/issues/review", 7),
                    "label": "Review draft",
                    "kind": "task"
                }
            }),
        }
    }

    #[test]
    fn a_task_run_needs_a_known_version_and_broker_bound_task_context() {
        {
            assert!(validate_task_run(&task_run()).is_ok());
            let mut mismatched = task_run();
            mismatched.action_context = serde_json::json!({
                "purpose": "Review draft",
                "group": {"id": "different", "label": "Review draft", "kind": "task"}
            });
            assert!(validate_task_run(&mismatched).is_err());
        }

        {
            let mut invalid = task_run();
            invalid.version = 2;
            assert!(validate_task_run(&invalid).is_err());
        }

        {
            let mut unleased = task_run();
            unleased.lease = "broker-minted-lease".into();
            assert!(validate_task_run(&unleased).is_err(), "only a native lease");
            let mut no_issue = task_run();
            no_issue.task_id = "task:review".into();
            assert!(
                validate_task_run(&no_issue).is_err(),
                "every run is an issue's"
            );
        }
    }

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
