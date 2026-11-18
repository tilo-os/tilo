//! One agent session: join the workspace and answer the conversations it is asked in.
//!
//! A turn the host hands this agent is answered through the workspace's MCP tools
//! ([`tilo_agent_turn`]): the device's session lists them and takes each call on this agent's
//! connection to it, as the turn's, in the agent's session ([`turn`]). What the turn does and how it ended is told in the conversation that
//! asked ([`conversation`]).

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use ui::time::Duration;

use anyhow::Result;
use tilo_actions::remote::Caller;
use tilo_agent_turn::{Carried, SessionActions};
use ui::collaboration::{CollabEvent, CollabRequest, PresencePayload, RouteSpec};
use ui::presence::PresenceKey;
use uuid::Uuid;

use crate::providers::{ProviderSelection, agent_selection};
use crate::wire::Link;

mod conversation;
mod selection_prompt;
mod summary;
mod turn;

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
        // The answer's result is its reply: what the person reads in the conversation, and what
        // whoever asked the agent through the API gets back.
        result: tilo_actions::ResultSpec::Json {
            schema: serde_json::json!({
                "type": "object",
                "properties": {"reply": {"type": "string"}},
                "required": ["reply"],
            }),
        },
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
    /// Let a model answer turns. False answers each turn that no model is set up.
    pub model: bool,
    /// How long one round of the model may take.
    pub model_timeout: Duration,
    /// The agent's content store, created once when it starts: what the web tools read is kept
    /// here.
    pub content: ui::content::ContentStore,
    /// The model the desktop chose for this agent, when it launched it with
    /// one; otherwise the Settings default this process can read, if any.
    pub provider: Option<ProviderSelection>,
    /// The certificate this connection is admitted with, when it is given one
    /// rather than inheriting the launcher's. A harness that runs an agent
    /// against a real broker over an explicit endpoint presents the same
    /// certificate a person's app does, so the agent is admitted as the agent
    /// the workspace keeps — never as an anonymous diagnostic peer.
    pub identity: Option<ui::collaboration::CertifiedEndpointIdentity>,
}

/// What makes the model a turn asks, given the switch that stops it.
pub type MakeModel = Arc<dyn Fn(Arc<AtomicBool>) -> Box<dyn tilo_agent_turn::Model> + Send + Sync>;

pub struct Session {
    cfg: Config,
    link: Link,
    /// The model this agent answers with, when it has one.
    selection: Option<ProviderSelection>,
    /// What makes each turn's model: the gateway's over `selection`, or a harness's.
    model: Option<MakeModel>,
    /// The last sequence number the turn's answer reported under.
    reported: u64,
    /// The broker's pushed, permission-filtered action directory: where the conversation's own
    /// actions on the turn's message are found.
    broker_catalogue: ui::collaboration::ActionCatalogue,
    /// The turn the run in progress answers, as its reply is being told.
    record: Option<conversation::Record>,
    /// The run's working resource of the failure that ended its turn, kept for whoever debugs
    /// it later; the reply's own words are the failure's sentence, never this.
    failure_detail: Option<String>,
    /// Whom this session asks a single question outside a turn's own exchange with its model:
    /// what a conversation's summary is kept with ([`summary`]).
    single_shot: Arc<dyn crate::transform::TransformModel>,
    /// Where the turn's tools and the conversation's steps run.
    runtime: tokio::runtime::Runtime,
    /// The session's action API, asked on this agent's link.
    actions: SessionActions,
    /// What the actions leave for the link to carry, and hand back what came.
    carried: Arc<Carried>,
    /// The agent's own web tools, kept from one turn to the next.
    web: Arc<std::sync::Mutex<Option<tilo_agent_web_tools::Web>>>,
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
        if managed.is_none() {
            // A managed agent is limited to its task-bound broker surface: the broker refuses the
            // cluster route, the late-join snapshot and presence on that descriptor, and one
            // refused snapshot RPC shuts down the whole collaboration bridge.
            link.join_route(RouteSpec::cluster(cfg.cluster));
            link.send(&CollabRequest::RequestDocumentSnapshot)?;
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
            name = %cfg.name,
            "joined the workspace"
        );
        let selection = match managed {
            Some(selection) => Some(selection),
            None if cfg.model => cfg
                .provider
                .clone()
                .or_else(|| agent_selection(&cfg.agent_id)),
            None => None,
        };
        Self::over(cfg, link, selection)
    }

    /// A session over `link`, answering with `selection`'s model.
    fn over(cfg: Config, link: Link, selection: Option<ProviderSelection>) -> Result<Self> {
        let single_shot = Arc::new(crate::transform::GatewayModel {
            launched: selection.clone(),
        });
        let model = selection.clone().map(|selection| {
            Arc::new(move |stop| {
                Box::new(crate::model::GatewayModel {
                    selection: selection.clone(),
                    stop,
                }) as Box<dyn tilo_agent_turn::Model>
            }) as MakeModel
        });
        let ring = link.ringer();
        let (actions, carried) = tilo_agent_turn::carried(cfg.cluster.into(), move || {
            if let Some(ring) = &ring {
                let _ = ring.send(());
            }
        });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("tilo-agent-turn")
            .enable_all()
            .build()?;
        Ok(Self {
            cfg,
            link,
            selection,
            model,
            reported: 0,
            broker_catalogue: ui::collaboration::ActionCatalogue::default(),
            record: None,
            failure_detail: None,
            single_shot,
            runtime,
            actions,
            carried,
            web: Arc::default(),
        })
    }

    /// Answer the turns the broker delivers, and the AI transforms it asks of this agent.
    pub fn run(&mut self) -> Result<()> {
        let mut transforms = crate::transform::TransformService::new(
            Arc::new(crate::transform::GatewayModel {
                launched: self.selection.clone(),
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
                Some(CollabEvent::RemoteAction { message }) => self.carried.hear(message),
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

    /// Apply a change to the broker catalogue. A change that does not apply leaves the catalogue
    /// as it was: the connection asks the broker for the whole catalogue, whose answer comes as a
    /// reset.
    fn apply_broker_catalogue(&mut self, change: &ui::collaboration::CatalogueChange) {
        if let Err(error) = self.broker_catalogue.apply(change) {
            tracing::warn!(%error, "the broker's catalogue change did not apply");
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
            if let Err(report_error) = self.fail_task(&asked, &error.to_string(), None) {
                tracing::warn!(%report_error, "the turn's failure was not reported");
            }
        }
        self.link.forget_cancelled(id);
        Ok(())
    }

    /// Answer with what `make` makes from now on, in place of the model this session connected
    /// with: how a harness scripts the model a turn asks, while every other part of the run stays
    /// the production one.
    pub fn set_model(&mut self, selection: ProviderSelection, make: MakeModel) {
        self.selection = Some(selection);
        self.model = Some(make);
    }

    /// Ask single questions of `model` from now on, in place of the gateway this session
    /// connected with: how a harness scripts them.
    pub fn set_single_shot(&mut self, model: Arc<dyn crate::transform::TransformModel>) {
        self.single_shot = model;
    }

    fn run_task(&mut self, run: &Asked) -> Result<()> {
        // What this run says is the person's language as it stands now: they
        // may have changed it since the app started.
        crate::i18n::follow();
        self.reported = 0;
        self.failure_detail = None;
        let cause = match conversation::validate(run) {
            Ok(cause) => cause,
            Err(message) => {
                tracing::warn!(id = run.id, %message, "refused a turn that answers no message");
                return self.report_task(run, TurnUpdate::Failed { message });
            }
        };
        self.record = None;
        self.start_turn(cause);
        self.report_task(run, TurnUpdate::Accepted)?;
        let (Some(selection), Some(model)) = (self.selection.clone(), self.model.clone()) else {
            let key = if self.cfg.model {
                "agent-plan-no-model"
            } else {
                "agent-plan-models-off"
            };
            return self.fail_task(run, &crate::i18n::t(key), None);
        };
        if let Some(refusal) = crate::model::refuses_tasks(&selection) {
            return self.fail_task(run, &refusal, None);
        }
        self.report_task(
            run,
            TurnUpdate::Progress {
                message: crate::i18n::t_args(
                    "agent-plan-model",
                    &[
                        (
                            "model",
                            ui::i18n::ArgValue::from(selection.model.display_label()),
                        ),
                        (
                            "provider",
                            ui::i18n::ArgValue::from(selection.profile_label.clone()),
                        ),
                    ],
                ),
            },
        )?;
        match self.work(run, &selection, &model)? {
            tilo_agent_turn::Outcome::Answered(reply) => self.succeed_turn(run, reply),
            tilo_agent_turn::Outcome::Stopped(why) | tilo_agent_turn::Outcome::Failed(why) => {
                self.fail_task(run, &why, None)
            }
            tilo_agent_turn::Outcome::Cancelled => self.cancel_task(run),
        }
    }

    /// Tell the conversation and the broker how `run` stands: the reply to its message shows it,
    /// and the turn's answer reports it.
    fn report_task(&mut self, run: &Asked, update: TurnUpdate) -> Result<()> {
        self.show_turn(run, &update)?;
        // How the turn ended, with what the person reads, once: the resource a run's reader finds
        // its end by.
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
            // The answer's result is its reply (`answer_descriptor`), as its contract declares.
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

    /// Record a failed turn on the reply to the run's message, and tell the host it failed:
    /// `message` is what a person reads, `detail` what the run keeps for whoever debugs it.
    fn fail_task(&mut self, run: &Asked, message: &str, detail: Option<&str>) -> Result<()> {
        self.failure_detail = detail.map(str::to_owned);
        self.report_task(
            run,
            TurnUpdate::Failed {
                message: message.to_owned(),
            },
        )
    }

    fn cancel_task(&mut self, run: &Asked) -> Result<()> {
        self.report_task(
            run,
            TurnUpdate::Cancelled {
                message: crate::i18n::t("agent-cancel-thinking"),
            },
        )
    }

    /// The caller this agent's own requests name; the host decides each as the turn it answers.
    fn caller(&self) -> Caller {
        Caller::local_person(self.cfg.agent_id.clone()).in_workspace(self.cfg.cluster.to_string())
    }

    /// Run `future` on the session's runtime while this thread carries what it asks on the link
    /// and hands back what the host answers, until it is done. While `watching` a turn, a stop the
    /// broker asks for sets its switch, and what the turn tells is said as it comes.
    fn carry<T: Send + 'static>(
        &mut self,
        future: impl Future<Output = T> + Send + 'static,
        watching: Option<&Watching<'_>>,
    ) -> Result<T> {
        let (done, outcome) = mpsc::channel();
        let ring = self.link.ringer();
        self.runtime.spawn(async move {
            let _ = done.send(future.await);
            if let Some(ring) = ring {
                let _ = ring.send(());
            }
        });
        loop {
            for message in self.carried.take_outbox() {
                self.link.send(&CollabRequest::RemoteAction { message })?;
            }
            for event in self.link.take_queued(|event| {
                matches!(
                    event,
                    CollabEvent::RemoteAction { .. } | CollabEvent::ActionCatalogue { .. }
                )
            }) {
                match event {
                    CollabEvent::RemoteAction { message } => self.carried.hear(message),
                    CollabEvent::ActionCatalogue { change } => self.apply_broker_catalogue(&change),
                    _ => {}
                }
            }
            if let Some(watching) = watching {
                if self.link.cancelled(watching.run.id) {
                    watching.stop.store(true, Ordering::SeqCst);
                }
                while let Ok(told) = watching.told.try_recv() {
                    self.tell(watching.run, told)?;
                }
            }
            match outcome.try_recv() {
                Ok(value) => return Ok(value),
                Err(mpsc::TryRecvError::Disconnected) => {
                    anyhow::bail!("the agent's work ended without an answer")
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if let Some(reason) = self.link.closed() {
                anyhow::bail!("the broker closed the agent's session: {reason}");
            }
            self.link.wait(CARRY_WAIT);
        }
    }
}

/// The longest one wait for the link sleeps while work is carried, should nothing ring it.
const CARRY_WAIT: Duration = Duration::from_millis(20);

/// A turn being carried: its stop switch, and what it tells as it works.
struct Watching<'a> {
    run: &'a Asked,
    stop: &'a AtomicBool,
    told: &'a mpsc::Receiver<turn::Told>,
}

/// The model selection of a launcher-managed agent, or `None` when this
/// agent was not launched with a managed context.
fn managed_context(cfg: &Config) -> Result<Option<ProviderSelection>> {
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
mod tests;
