//! The one engine that runs a workspace's agents: on the workspace's host, around the clock, for
//! the workspaces it governs, and on a person's own device for the workspaces it holds
//! ([`on_device`]).
//!
//! Who they are is the workspace's own record of its agents (Assistant's
//! `tilo.workspace.agent`): every active agent no MCP client connects as works here, whoever
//! made it. What they are asked is what the workspace's people
//! write: a message in a conversation the agent takes part in or that names it
//! ([`chat::summons`]), and an issue given to the agent
//! or written in once it has it ([`tasks_model::IssueAsk`]). A message asks too when something
//! else started the turn for a person (a reminder that is due, an event that fired) through
//! Chat's `ask-agent`: the agent then acts for the person its cause names ([`chat::cause`]). The
//! records hold every ask and every answer, so the host keeps nothing of its own: a host that
//! restarts finds the same asks.
//!
//! A turn starts from the agent's triggers ([`triggered`]): a change one of them fires on
//! invokes the agent's answer, and the engine takes the turn from that change alone
//! ([`ServerAgents::run`]). Nothing scans the workspace for what is owed.
//!
//! An agent has one turn at a time in a conversation ([`going`]). What its person writes while
//! it works reaches that turn at its next step: it is told it and goes on under it, where its
//! answer then stands. What a cause asks waits until the turn ended. The person a turn is for
//! stops it (Chat's `stop-agent`): it ends at its next step, a model call under way included,
//! and its reply says it was stopped. A turn a host that stopped left begun is not started
//! over, since its steps may have changed things already: its reply says it was interrupted,
//! and asking again is the person's to do. One it had only taken runs, once.
//!
//! A look reads what a workspace owes first and acts on what it read a moment later, when a
//! turn may have ended or gone on. So a turn begins only what the workspace still owes once it
//! holds its place, and a turn that goes is handed only what was read since it began or last
//! went on ([`going::Reading`]): no message is asked of a model twice, and none is left for an
//! older one.
//!
//! An agent works inside a turn: the person's ask. It acts through the same agent tools an MCP
//! client gets ([`McpServer::for_turn`]), over the host's own action service, as the person's
//! device handing its session to the agent on that turn: everything it does is held to what the
//! person may do here now, capped as every agent is and narrowed by the bound its record keeps, and
//! an action that asks first is put to the person on their own devices. Its changes are its own in
//! history, for the person. Its model is the workspace's: the one the workspace linked for the
//! work its agents do, asked with the workspace's key ([`Workspaces::workspace_model`]), and never
//! another once it linked any. A workspace that linked none is told what to link. What each
//! turn costs is counted on the agent's record, and an agent whose daily limit is reached stops
//! and says so.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, ChangeFeed, ChangePage,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
    RemoteActionMessage, RemoteActionTarget, WatchScope, pending,
};
use tilo_actions::{InvocationId, InvocationUpdate, api::ActionAddress};
use tilo_agent_session::model::{Answered as ModelAnswered, PlanTurns};
use tilo_agent_session::resource_provider;
use tilo_lock::MutexRecover as _;
use tilo_mcp::McpServer;
use tilo_model_gateway::host_backend::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, StreamEvent, ToolCall,
    ToolDefinition,
};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_resource::Resource as _;
use tilo_server_protocol::WorkspaceId;
use tilo_workspace_agent::{Agent, WorkspaceAgents};
use tracing::Instrument as _;

use crate::workspaces::Workspaces;
use crate::workspaces::person_model::{ModelRefusal, PersonModel, Spender};
use events::{Authority, WorkspaceEvents};
use initiative::Initiative;
use tilo_actions::remote::workspace_events;
use tilo_actions::remote::workspace_initiative;
use turn_provider::TurnProvider;

pub mod agent_keys;
pub mod assistant;
pub mod events;
pub mod initiative;
pub mod kept;
pub mod telling;
pub mod triggered;

mod across;
mod answers;
mod asking;
mod asks;
mod calendar_connect;
mod calendars;
mod carried;
#[cfg(test)]
mod device_turn_tests;
mod engine_actions;
mod going;
mod instructions;
mod issue_work;
mod learned;
mod mail;
mod mail_connect;
mod narration;
mod shares;
mod steps;
mod sub_agents;
mod summary;
mod task_waits;
mod team;
mod tl_tasks;
mod turn_provider;
mod vault_uses;
pub mod web_search;
mod written;

// What a turn reaches of the services its person connected.
mod connections;
// A turn's open of Browser, where this server places the window.
mod turn_browser;
// What a turn does at the page in that window.
mod turn_pages;
// The Browser windows a host places, as a turn reaches them.
pub mod windows;

pub use asks::{Answered, Carried, Carries, ServerAsk};
pub use engine_actions::EngineActions;
pub use sub_agents::Line;
/// The engines, and a server on this machine that answers as they and the pages do, for a
/// test's web tools.
#[cfg(any(test, feature = "fixture-servers"))]
pub use tilo_web_search::{Engine, fixtures};
#[cfg(feature = "apps")]
pub use web_search::{PROVIDER as WEB_SEARCH_PROVIDER, WebSearch};

/// What a turn's reply says when the turn goes on with what its person wrote next.
const WENT_ON: &str = "I am going on with what you wrote next, and answer below.";
/// What the reply says of a turn that a server which stopped left begun.
const LEFT_BEGUN: &str = "I was interrupted while I worked on this: the server restarted after I \
                          had started. I did not start over, so that nothing is done twice. Ask \
                          again and I begin anew.";

/// The app that keeps the conversations.
const CHAT: &str = "so.tilo.chat";
/// The app that keeps the workspace's list of its agents.
const AGENTS_OWNER: &str = tilo_workspace_agent::OWNER;
/// The app that keeps the issues.
const TASKS: &str = "so.tilo.tasks";
/// The most times one look reads what a workspace owes: it reads again where a turn began or
/// went on under what it read ([`going::Reading`]).
const READINGS: usize = 3;
/// How long one model turn may take.
const TURN_TIME: Duration = Duration::from_secs(180);
/// The most tokens one model answer may take.
const ANSWER_TOKENS: u32 = 4096;
/// The characters a turn's conversation keeps before old answers give way, for the model that
/// says nothing of its own context ([`tilo_agent_turn::Asked::new`]'s bound): a model whose
/// descriptor knows its window sizes the conversation to it instead.
const ASKED_CONTEXT_CHARS: usize = 400_000;
/// The longest a tool's answer the model is shown, in bytes.
const MOST_RESULT_BYTES: usize = 24 * 1024;
/// How long the host waits for one of its own record writes (the reply, the comment) to be taken.
const WRITE_WAIT: Duration = Duration::from_secs(30);
/// The tool a turn in a conversation answers with a view through: Chat's own
/// `answer-with-view`, on the reply to the turn's ask.
const ANSWER_WITH_VIEW: &str = "answer_with_view";

/// Chat's action that keeps on a reply what its agent says of its work.
const KEEP_WORK: &str = "record-work";

/// The provider serving one workspace's actions as an [`ActionProvider`]: what the agent's
/// tools run on, and what its replies are written through.
#[derive(Clone)]
pub struct WorkspaceActionProvider {
    actions: EngineActions,
    workspace: WorkspaceId,
    /// The events this device runs for its workspaces, where it runs any: what an agent sets up,
    /// pauses, changes and removes for the person its turn is for.
    events: Option<WorkspaceEvents>,
    /// What the workspaces' assistants find to do on their own, where this server keeps it: the
    /// look-round an agent reads, turns on and off for the person its turn is for.
    initiative: Option<Initiative>,
    /// What this service answers itself for the one turn it serves, beside the workspace's
    /// resources ([`TurnProvider`]).
    own: Vec<Arc<dyn TurnProvider>>,
    /// How the calls those took ended; `None` while it answers none of them.
    ended: Option<turn_provider::Ended>,
    /// What its own services answered, for a caller that follows an invocation.
    answered: OwnAnswers,
    /// What the person asked of the turn this serves, in their words: the purpose History
    /// keeps with each change the turn makes, so its agent session reads as what it was asked.
    purpose: Option<String>,
}

/// What the services answered here have said, kept for whoever follows the invocation. They
/// answer with the receipt alone and the workspace's actions never hear of it, while a turn's
/// tools read how an invocation ended by observing it: without this an action that was carried
/// out is told to the model as one still pending.
#[derive(Clone, Default)]
struct OwnAnswers(Arc<Mutex<VecDeque<(InvocationId, Caller, InvocationUpdate)>>>);

impl OwnAnswers {
    /// The most answers kept. A turn's tools observe an invocation as soon as it is admitted,
    /// so only the newest are ever asked for.
    const KEPT: usize = 64;

    /// Keep how `receipt` says `caller`'s invocation ended. One that admitted nothing, or
    /// says nothing of the end yet, leaves nothing to follow.
    fn keep(&self, caller: &Caller, receipt: &InvocationReceipt) {
        let invocation = receipt.invocation;
        let update = match &receipt.status {
            ReceiptStatus::Completed { output } => InvocationUpdate::Succeeded {
                invocation,
                output: output.clone(),
            },
            ReceiptStatus::Failed { error } => InvocationUpdate::Failed {
                invocation,
                error: error.clone(),
            },
            ReceiptStatus::Cancelled { message } => InvocationUpdate::Cancelled {
                invocation,
                message: message.clone(),
            },
            ReceiptStatus::Rejected { .. } | ReceiptStatus::Accepted => return,
        };
        let mut kept = self.0.lock_recover();
        if kept.len() >= Self::KEPT {
            kept.pop_front();
        }
        kept.push_back((invocation, caller.clone(), update));
    }

    /// How invocation `id` ended, for the `caller` that made it and nobody else.
    fn of(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.0
            .lock_recover()
            .iter()
            .rev()
            .find(|(invocation, by, _)| invocation == id && by == caller)
            .map(|(_, _, update)| InvocationState::new(*id, update.clone()))
    }
}

impl WorkspaceActionProvider {
    #[must_use]
    pub fn new(actions: impl Into<EngineActions>, workspace: WorkspaceId) -> Self {
        Self {
            actions: actions.into(),
            workspace,
            events: None,
            own: Vec::new(),
            ended: None,
            initiative: None,
            answered: OwnAnswers::default(),
            purpose: None,
        }
    }

    /// Another's write landed in `route` of this workspace, as this server just read it in the
    /// workspace's store: the next request made through this service acts on it
    /// ([`EngineActions::landed`]).
    pub fn landed(&self, route: DocumentRoute) {
        self.actions.landed(self.workspace, route);
    }

    /// What `caller` is offered of the workspace's records, its events and
    /// its assistant's initiative, as the workspace's policy decides it for a
    /// turn this server carries out for them.
    async fn page(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Result<CataloguePage, ActionCallError> {
        if let Some(events) = &self.events
            && query.provider.as_deref() == Some(workspace_events::PROVIDER)
        {
            return Ok(events
                .catalogue(self.workspace, caller, &Authority::Turn, &query)
                .await);
        }
        if let Some(initiative) = &self.initiative
            && query.provider.as_deref() == Some(workspace_initiative::PROVIDER)
        {
            return Ok(initiative
                .catalogue(self.workspace, caller, &Authority::Turn, &query)
                .await);
        }
        let initiative = match &self.initiative {
            Some(initiative) if query.provider.is_none() => {
                initiative
                    .provider(self.workspace, caller, &Authority::Turn)
                    .await
            }
            _ => None,
        };
        // A listing of everything names the events' provider beside the records'.
        let events = match &self.events {
            Some(events) if query.provider.is_none() => {
                events
                    .provider(self.workspace, caller, &Authority::Turn)
                    .await
            }
            _ => None,
        };
        // One provider id is answered by every provider of the turn's own that holds a
        // resource of it: their rows make one page.
        let own: Vec<_> = self
            .own
            .iter()
            .filter_map(|own| own.rows(caller, &query))
            .collect();
        if !own.is_empty()
            && let Some(ended) = &self.ended
        {
            return ended
                .page(own, query)
                .await
                .unwrap_or_else(|| Err(unavailable()));
        }
        // A listing of everything names the turn's own providers beside the workspace's, each
        // once.
        let mut offered: Vec<tilo_actions::remote::CatalogueProvider> = Vec::new();
        if query.provider.is_none() {
            for named in self.own.iter().flat_map(|own| own.providers(caller)) {
                if offered.iter().all(|known| known.provider != named.provider) {
                    offered.push(named);
                }
            }
        }
        match self
            .serve(
                caller,
                RemoteActionMessage::CatalogueRequest {
                    correlation: 0,
                    query,
                },
            )
            .await
        {
            Some(RemoteActionMessage::CatalogueResponse { result, .. }) => {
                result.map(|mut page| {
                    page.providers.extend(events);
                    page.providers.extend(offered);
                    page.providers.extend(initiative);
                    page
                })
            }
            _ => Err(unavailable()),
        }
    }

    /// This service, for a turn its person asked `asked` of: every invocation it carries out
    /// that says no purpose of its own has that one ([`tilo_actions::ActionContext::purpose`]).
    #[must_use]
    fn serving_ask(mut self, asked: &str) -> Self {
        let asked = asked.trim();
        self.purpose = (!asked.is_empty()).then(|| asked.to_owned());
        self
    }

    /// This service, answering also for `initiative`: the server's own actions on what a
    /// person's assistant finds to do on its own.
    #[must_use]
    pub fn offering_initiative(mut self, initiative: Option<Initiative>) -> Self {
        self.initiative = initiative;
        self
    }

    /// This service, answering also for `events`: this server's own actions on the workspace's
    /// scheduled work and events, listed and taken beside its resources.
    #[must_use]
    pub fn offering(mut self, events: Option<WorkspaceEvents>) -> Self {
        self.events = events;
        self
    }

    /// This service, answering `provider` as well for the turn it serves: named in what the
    /// turn's tools list, and called like the workspace's own actions.
    #[must_use]
    fn answering(mut self, provider: Arc<dyn TurnProvider>) -> Self {
        self.own.push(provider);
        if self.ended.is_none() {
            self.ended = Some(turn_provider::Ended::here());
        }
        self
    }

    fn serve(
        &self,
        caller: &Caller,
        mut message: RemoteActionMessage,
    ) -> Pending<RemoteActionMessage> {
        // What a tool call asks of the workspace's action service is counted for its step.
        let asking = steps::asking(&message);
        // What the agent asks of the workspace it asks under its own key.
        if let RemoteActionMessage::Invoke(request) = &mut message
            && let Err(reason) = self.actions.sign_for(self.workspace, caller, request)
        {
            return Pending::ready(RemoteActionMessage::Receipt(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Rejected {
                    error: ActionCallError::Refused { reason },
                },
            }));
        }
        let answer = self
            .actions
            .serve(self.workspace, caller, message)
            .unwrap_or_else(|| {
                Pending::ready(RemoteActionMessage::CatalogueResponse {
                    correlation: 0,
                    result: Err(unavailable()),
                })
            });
        match asking {
            Some(asking) => asking.answered(answer),
            None => answer,
        }
    }
}

fn unavailable() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: "the provider's workspace actions are not running".into(),
    }
}

impl ActionProvider for WorkspaceActionProvider {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return Pending::ready(Err(unavailable()));
        };
        let (completer, answer) = pending();
        let (this, caller) = (self.clone(), caller.clone());
        // The listing is counted for the tool call that asks for it, though it runs apart.
        runtime.spawn(steps::carried(async move {
            completer.complete(this.page(&caller, query).await);
        }));
        answer
    }

    fn invoke(
        &self,
        caller: &Caller,
        mut request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if let Some(purpose) = &self.purpose {
            let context = request.context.get_or_insert_with(Default::default);
            if context.purpose.is_none() {
                context.purpose = Some(purpose.clone());
            }
        }
        if let Some(reason) = shares::refuses(caller, &request) {
            return Pending::ready(Err(ActionCallError::InvalidRequest { reason }));
        }
        if let Some(events) = &self.events
            && request.target.provider == workspace_events::PROVIDER
        {
            // Kept whoever still waits: a tool that stopped waiting asks later how it went.
            let (answered, by) = (self.answered.clone(), caller.clone());
            let (completer, answer) = pending();
            events
                .invoking(self.workspace, caller, Authority::Turn, request)
                .then(move |receipt| {
                    if let Some(receipt) = receipt {
                        answered.keep(&by, &receipt);
                        completer.complete(Ok(receipt));
                    }
                });
            return answer;
        }
        if let Some(initiative) = &self.initiative
            && request.target.provider == workspace_initiative::PROVIDER
        {
            // Kept whoever still waits: a tool that stopped waiting asks later how it went.
            let (answered, by) = (self.answered.clone(), caller.clone());
            let (completer, answer) = pending();
            initiative
                .invoking(self.workspace, caller, Authority::Turn, request)
                .then(move |receipt| {
                    if let Some(receipt) = receipt {
                        answered.keep(&by, &receipt);
                        completer.complete(Ok(receipt));
                    }
                });
            return answer;
        }
        if let Some(own) = self.own.iter().find(|own| own.takes(&request.target))
            && let Some(ended) = &self.ended
        {
            return ended.carry_out(Arc::clone(own), caller, request);
        }
        self.serve(caller, RemoteActionMessage::Invoke(request))
            .map(|answer| match answer {
                RemoteActionMessage::Receipt(receipt) => Ok(receipt),
                _ => Err(unavailable()),
            })
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        // What a provider of the turn's own took is no invocation of the workspace's: how it
        // ended is kept here.
        self.ended
            .as_ref()
            .and_then(|ended| ended.of(id))
            .or_else(|| self.actions.observe(caller, id))
            .or_else(|| self.answered.of(caller, id))
    }

    fn observed(&self, _caller: &Caller, id: &InvocationId, state: Option<&InvocationState>) {
        steps::looked(id, state);
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        self.serve(caller, RemoteActionMessage::Cancel { invocation: *id })
            .map(|answer| match answer {
                RemoteActionMessage::CancelResponse { result, .. } => result,
                _ => Err(unavailable()),
            })
    }

    /// The next time what a watch of the caller's workspace follows may have changed: the
    /// workspace's own service holds that wait, answered by a write landing in its replica of
    /// the resources, whoever wrote it, and by who is in the workspace moving with its
    /// governance. A watch of a turn on this server is woken by a person's edit or message and
    /// by another agent's change, not only by its own invocations.
    fn changed(
        &self,
        caller: &Caller,
        scope: WatchScope,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        // The service's refusal is the watch's to tell, with why. A service that answers nothing
        // at all is not running, and the watch waits its own deadline instead of on a wake that
        // would come at once for ever.
        self.actions
            .serve(
                self.workspace,
                caller,
                RemoteActionMessage::ChangedRequest {
                    correlation: 0,
                    scope,
                },
            )
            .map(|answer| {
                answer.map(|message| match message {
                    RemoteActionMessage::ChangedResponse { result, .. } => result,
                    other => Err(ActionCallError::Unavailable {
                        reason: format!(
                            "the workspace answered a wait for a change with {other:?}"
                        ),
                    }),
                })
            })
    }

    /// One document of the workspace, read as every reader of it is answered: the same read a
    /// person's device answers, from what the workspace's service holds.
    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        self.actions.resource_reads()
    }

    /// The workspace's own search, asked as whoever calls it: what an agent's `find` answers
    /// with on this server.
    fn resource_search(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceSearch>> {
        Some(Arc::new(self.clone()))
    }

    /// The workspace's own feed of its resources' changes, asked as whoever calls it: what a
    /// watch of the whole workspace reads on this server, and only what that caller may read
    /// of them.
    fn change_feed(&self) -> Option<Arc<dyn ChangeFeed>> {
        Some(Arc::new(self.clone()))
    }
}

/// What a search answers with, when it has.
type Searched = Pending<Result<Vec<tilo_actions::remote::Found>, ActionCallError>>;

/// What a turn on this server finds in what its workspace holds.
///
/// The workspace's action service searches what the workspace holds for every device that asks
/// it ([`tilo_actions::remote::search::FIND`]): each resource by what its app tells search
/// about it, and the files of the workspace's folder. A turn this server carries out asks the
/// same service the same question, as the caller its tools act as: the person's session,
/// handed to the agent on that turn with the rules the turn is decided by. So a turn finds what
/// that caller may read and nothing else, decided where every other caller's search is decided,
/// and nothing is searched a second way here. Narrowed to a
/// conversation or to a stretch of time ([`tilo_actions::remote::Within`]) it is the same
/// question still.
impl tilo_actions::remote::ResourceSearch for WorkspaceActionProvider {
    fn find(&self, caller: &Caller, needle: &str) -> Searched {
        self.find_within(caller, needle, &tilo_actions::remote::Within::default())
    }

    fn find_within(
        &self,
        caller: &Caller,
        needle: &str,
        within: &tilo_actions::remote::Within,
    ) -> Searched {
        use tilo_actions::remote::search;
        let request = search::Query::request(InvocationId::new(), needle, within);
        self.serve(caller, RemoteActionMessage::Invoke(request))
            .map(|answer| match answer {
                RemoteActionMessage::Receipt(receipt) => search::found(receipt),
                _ => Err(unavailable()),
            })
    }
}

/// What a watch of the whole workspace reads on this server: every change to everything its
/// workspace holds, as the service that keeps it saw it, for the caller its tools act as. The
/// service answers only what that caller may read, so an agent is told of a person's edit or a
/// bot's change in what it reads, and of nothing in what its rules keep from it.
impl ChangeFeed for WorkspaceActionProvider {
    fn since(
        &self,
        caller: &Caller,
        after: Option<&str>,
    ) -> Pending<Result<ChangePage, ActionCallError>> {
        self.serve(
            caller,
            RemoteActionMessage::ChangesRequest {
                correlation: 0,
                after: after.map(str::to_owned),
            },
        )
        .map(|answer| match answer {
            RemoteActionMessage::ChangesResponse { result, .. } => result,
            _ => Err(unavailable()),
        })
    }
}

/// Run the agents of the workspaces a person's own device holds, in the device's own workspace
/// service once `service` holds it: the one engine a host runs, in this one daemon, with its
/// turns' calls decided by the device's broker (`actions`) as the person's own calls are. It
/// takes every turn its workspaces' records ask of their agents that is this device's to take
/// ([`ServerAgents::run`], [`crate::work::takes_turn`]).
pub async fn on_device(
    service: crate::service::ServiceSlot,
    actions: Arc<dyn ActionProvider + Send + Sync>,
) {
    let workspaces = service.installed().await.workspaces().clone();
    run_on_device(workspaces, actions).await;
}

/// [`on_device`] for the service whose `workspaces` the device opened already.
pub async fn run_on_device(workspaces: Workspaces, actions: Arc<dyn ActionProvider + Send + Sync>) {
    // The person's own device calls the model their workspace linked where it is, one on their
    // own network or on this machine included: a local model answers with no network at all.
    workspaces.call_private_model_addresses(true);
    // The model this computer's Agent models pane chose is the workspaces' it keeps.
    workspaces.on_a_computer();
    workspaces.follow_this_computer();
    telling::start(&workspaces);
    // Each agent signs its calls with a key of its own, made here from its person's root.
    let keys = match agent_keys::FleetAgentKeys::from_env() {
        Ok(keys) => Arc::new(keys),
        Err(error) => {
            tracing::warn!("this device runs no agents: its identity store is not open ({error})");
            return;
        }
    };
    tracing::info!(
        "this device runs its workspaces' agents, each on the model its workspace linked"
    );
    // The agents speak the language their person reads this computer in.
    ServerAgents::on_device(workspaces, actions, keys)
        .speaking(settings_store::appearance::preferred_language)
        .run()
        .await;
}

/// The language a host's agents speak, asked anew for each turn ([`ServerAgents::speaking`]).
pub type Language = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// What carries one turn's model calls, made fresh for each turn on its own thread: a test's
/// script, standing where a workspace's model would ([`ServerAgents::scripted`]).
pub type Model = Arc<dyn Fn() -> Option<Box<dyn PlanTurns>> + Send + Sync>;

/// Something a person asked an agent, and where its answer goes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ask {
    /// A message in a conversation: the answer is the reply Chat derives for it.
    Message {
        /// The part of Chat's messages that holds it.
        part: String,
        message: String,
        attempt: u32,
    },
    /// A change one of the agent's triggers fired on ([`triggered`]): the answer is written
    /// where the change was made, a reply in its conversation or a comment on its task.
    Triggered(triggered::Where),
    /// An issue given to the agent, or written in once it had it: the answer is the agent's
    /// comment in its thread.
    Issue {
        /// The tracker that keeps it.
        record: String,
        issue: String,
        /// What asked: the person's message, or the assignment.
        cause: String,
    },
}

/// One turn the server owes an agent: who asks what, of whom.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub agent: Agent,
    /// The person who asked, by their person id.
    pub person: String,
    pub ask: Ask,
    /// The conversation the ask is in, by its address in the workspace.
    pub conversation: String,
    /// The ask's own id there.
    pub cause: String,
    /// What the person asked, in words.
    pub request: String,
    /// Earlier messages kept in the conversation, before this request.
    pub history: Vec<chat::recall::Recalled>,
    /// What they asked, as a person reads it: what the agent's session is labelled by.
    pub asked: String,
    /// What the turn asks its agent to take into its conversation's summary while it answers
    /// ([`chat::recall::told`]): none for most turns, and for every issue.
    pub fold: Option<runtime_collab::recall::Fold>,
    /// Whether the person wrote it themselves, rather than a cause asking for them: their own
    /// words reach a turn of theirs that is going.
    pub typed: bool,
    /// Where a sub-agent's turn stands in its team ([`Line`]): none for an agent a person made.
    pub line: Option<Line>,
}

/// The turns of a server's agents, as the server stops: it begins none, and waits for those going.
#[derive(Clone)]
pub struct Draining(Arc<going::Turns>);

impl Draining {
    /// Begin no turn any more, and wait until the turns going ended, `within` at most: whether
    /// they did. What their people write meanwhile stays in the workspace for the next start.
    pub async fn finish(&self, within: Duration) -> bool {
        self.0.close();
        let began = tilo_clock::now();
        let mut said = false;
        while !self.0.idle() {
            if began.elapsed() >= within {
                tracing::warn!(
                    going = self.0.going(),
                    "the server stops with agent turns still going: their replies say they were interrupted"
                );
                return false;
            }
            if !said {
                tracing::info!(
                    going = self.0.going(),
                    "the server waits for its agents' turns to end before it stops"
                );
                said = true;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        true
    }
}

/// What a workspace's records ask of the agents its host runs.
#[derive(Default)]
struct Asks {
    /// The turns they owe.
    turns: Vec<Turn>,
    /// The turns a person stopped whose work is not ended yet.
    stopped: Vec<Turn>,
    /// The turns their people's answers owe the agents that asked them.
    answered: Vec<answers::Wake>,
    /// What this server owes the workspace's sub-agents beside their turns.
    sub: sub_agents::Owed,
}

/// What the first step of a look read of a workspace: what it owes its agents, and where the
/// server's turns stood when the reading began. The look's second step enters with it.
pub struct LookRead {
    reading: going::Reading,
    asks: Asks,
}

/// What resolves the signing session an agent's turns are taken through: the host that
/// approved the sessions.
pub trait SessionDevices: Send + Sync {
    /// The device of the authenticated session that serves `agent`'s turns for `person` in
    /// `workspace`, among `sessions`: `None` where no approved session serves them.
    fn session_device<'a>(
        &'a self,
        sessions: &'a tilo_agent_session::AgentSessions,
        workspace: WorkspaceId,
        agent: &'a str,
        person: &'a [u8; 32],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<[u8; 32]>> + Send + 'a>>;
}

/// What each agent acts under: on a host, its own approved key, where its approval is read and
/// the signing sessions that hold the keys; on the person's device, the key that device makes
/// for it ([`agent_keys`]).
#[derive(Clone)]
enum Roster {
    Hosted(Arc<dyn SessionDevices>, tilo_agent_session::AgentSessions),
    Device(Arc<dyn agent_keys::AgentKeys>),
}

/// `agent` of `workspace` as its own key, working for `person`: what everything it does there
/// goes as, each invocation signed by that key.
///
/// # Errors
/// The agent belongs to no person; on a host, no approved signing session for the workspace; on a
/// device, no key of its own there.
async fn agent_caller(
    roster: &Roster,
    workspace: WorkspaceId,
    agent: &Agent,
    person: [u8; 32],
) -> Result<Caller, String> {
    let root = tilo_action_policy::PersonId::parse(&agent.acts_for)
        .ok()
        .and_then(|its| its.root())
        .ok_or("the agent belongs to no person")?;
    let key = match roster {
        Roster::Hosted(server, sessions) => server
            .session_device(sessions, workspace, &agent.id.0, &root)
            .await
            .filter(|device| {
                sessions.matches_roster(device, workspace, &agent.id.0, &root, agent.created_at)
            })
            .ok_or("the hosted agent has no approved signing session for this workspace")?,
        Roster::Device(keys) => keys.grant(workspace, &agent.id.0, &agent.name, root)?,
    };
    let caller = Caller::delegated_agent(agent.id.0.clone(), root, key)
        .in_workspace(workspace.to_string())
        .named(&agent.name)
        .on_roster(agent.created_at, person);
    Ok(match roster {
        // A host reads the agent's bound where it decides its approved key; on the person's
        // device the agent carries the one its record keeps, which the device's broker holds
        // it to.
        Roster::Hosted(..) => caller,
        Roster::Device(_) => caller.held_to(agent.turn_rules(&workspace.to_string())),
    })
}

/// Which agents an engine acts as: on a host, those its roster approved a signing session for;
/// on a person's device, those whose key the device makes from a root it holds. The agent's own
/// record says whose it is. With them, the agents whose answer the engine takes.
struct EngineKeys {
    roster: Roster,
    workspaces: crate::workspaces::WeakWorkspaces,
    /// The agents of the workspaces the engine runs, by id with their names, as it last read
    /// them: each one's answer is what its triggers invoke.
    answering: Arc<Mutex<Vec<(String, String)>>>,
    /// Where a trigger's answer reaches the engine's own loop ([`ServerAgents::run`]).
    answers: tokio::sync::mpsc::UnboundedSender<Answer>,
}

/// A turn a trigger asked of an agent, on its way to the engine's loop: what fired, the person
/// whose trigger it was, and where to say whether the turn was taken.
struct Answer {
    workspace: String,
    agent: String,
    event: tilo_automation::dispatch::DocumentEvent,
    saver: String,
    started: tilo_actions::remote::Completer<Result<crate::work::Started, String>>,
}

impl crate::work::AgentEngine for EngineKeys {
    fn acts_as<'a>(
        &'a self,
        workspace: &'a str,
        agent: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            let (Some(workspaces), Some(id)) =
                (self.workspaces.upgrade(), WorkspaceId::parse(workspace))
            else {
                return false;
            };
            let record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
            let Ok(Some(agents)) = workspaces
                .read_resource::<WorkspaceAgents>(workspace, (AGENTS_OWNER, record.as_str()))
                .await
            else {
                return false;
            };
            let Some(agent) = agents.agents.iter().find(|its| its.id.0 == agent) else {
                return false;
            };
            agent_caller(&self.roster, id, agent, [0; 32]).await.is_ok()
        })
    }

    fn answering(&self) -> Vec<(String, String)> {
        self.answering.lock_recover().clone()
    }

    fn answer(
        &self,
        workspace: String,
        agent: String,
        event: tilo_automation::dispatch::DocumentEvent,
        saver: String,
    ) -> Pending<Result<crate::work::Started, String>> {
        let (started, answer) = pending();
        let asked = Answer {
            workspace,
            agent,
            event,
            saver,
            started,
        };
        if let Err(refused) = self.answers.send(asked) {
            refused
                .0
                .started
                .complete(Err("this device's agents stopped".to_owned()));
        }
        answer
    }
}

/// The agents a host runs for the workspaces it governs.
#[derive(Clone)]
pub struct ServerAgents {
    workspaces: Workspaces,
    actions: EngineActions,
    roster: Roster,
    /// A test's script, answering in a workspace that linked no model: `None` outside tests,
    /// where such a workspace is told what to link.
    model: Option<Model>,
    /// The language the agents' own words are said in where this host knows its person's: a
    /// person's own computer speaks theirs ([`Self::speaking`]). `None` says them in the host's
    /// source language.
    language: Option<Language>,
    /// The turns going now: one for each agent in each conversation, with what reaches it
    /// while it works.
    going: Arc<going::Turns>,
    /// The events this host runs for its people, which a turn's tools reach for its person.
    events: Option<WorkspaceEvents>,
    /// What carries out the answers to what code of this server asked.
    carriers: asks::Carriers,
    /// What the people's assistants find to do on their own, which a turn's tools reach too.
    initiative: Option<Initiative>,
    /// The agents whose answer the engine takes, as it last read its workspaces' records
    /// ([`EngineKeys::answering`]).
    answering: Arc<Mutex<Vec<(String, String)>>>,
    /// What triggers asked of the agents, which [`Self::run`] takes.
    answers: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<Answer>>>>,
    /// The Browser windows this server places for its workspaces, where it places any: a
    /// turn's open of Browser has its window placed there ([`turn_browser`]), and a turn has
    /// a field of its workspace's page filled there from its person's vault.
    browsers: Option<Arc<dyn windows::Windows>>,
    /// The devices this server placed itself for its workspaces' windows, where it was told
    /// them: none of them is a device a read for a person acts as ([`across`]).
    placed: Option<crate::environment::EnvironmentRegistry>,
    /// Where a test's web tools ask instead of the public engines and pages: a server on this
    /// machine, per engine. `None` outside tests, where the tools reach the web.
    #[cfg(any(test, feature = "fixture-servers"))]
    web_routes: Option<Vec<(tilo_web_search::Engine, String)>>,
}

impl ServerAgents {
    /// Run the workspace roster through the host's approved signing sessions, which `devices`
    /// resolves.
    #[must_use]
    pub fn with_sessions(
        workspaces: Workspaces,
        actions: impl Into<EngineActions>,
        devices: Arc<dyn SessionDevices>,
        sessions: tilo_agent_session::AgentSessions,
    ) -> Self {
        Self::build(
            workspaces,
            actions.into(),
            None,
            Roster::Hosted(devices, sessions),
        )
    }

    /// The agents a person's own device runs for the workspaces it holds: their turns' calls go
    /// through the device's broker (`actions`), each signed by the agent's own key from `keys`.
    #[must_use]
    pub fn on_device(
        workspaces: Workspaces,
        actions: Arc<dyn ActionProvider + Send + Sync>,
        keys: Arc<dyn agent_keys::AgentKeys>,
    ) -> Self {
        Self::build(
            workspaces,
            EngineActions::Device(actions, Arc::clone(&keys)),
            None,
            Roster::Device(keys),
        )
    }

    fn build(
        workspaces: Workspaces,
        actions: EngineActions,
        model: Option<Model>,
        roster: Roster,
    ) -> Self {
        // The device takes the turns of its workspaces' agents that are its own from now on.
        let answering = Arc::<Mutex<Vec<(String, String)>>>::default();
        let (answers, asked) = tokio::sync::mpsc::unbounded_channel();
        workspaces.run_agents(Arc::new(EngineKeys {
            roster: roster.clone(),
            workspaces: workspaces.downgrade(),
            answering: Arc::clone(&answering),
            answers,
        }));
        let agents = Self {
            workspaces,
            actions,
            roster,
            model,
            language: None,
            going: Arc::default(),
            events: None,
            carriers: Arc::default(),
            initiative: None,
            answering,
            answers: Arc::new(Mutex::new(Some(asked))),
            browsers: None,
            placed: None,
            #[cfg(any(test, feature = "fixture-servers"))]
            web_routes: None,
        };
        // What a person answers about a connection of theirs is carried out from the start.
        agents.carries(
            connections::WORD,
            connections::answers(&agents.workspaces, &agents.roster),
        );
        // What a turn asks its person is put to them in its conversation.
        agents.ask_in_conversations();
        agents.carry_sends();
        agents
    }

    /// Say the agents' own words (why no model answered, what a provider's failure was) in the
    /// language `language` answers, asked at each turn: the person may change it at any time.
    #[must_use]
    pub fn speaking(
        mut self,
        language: impl Fn() -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.language = Some(Arc::new(language));
        self
    }

    /// The language the agents' own words are said in now ([`Self::speaking`]).
    fn language(&self) -> Option<String> {
        self.language.as_ref().and_then(|language| language())
    }

    /// A test's script answers in every workspace here that linked no model.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn scripted(mut self, model: impl Into<Option<Model>>) -> Self {
        self.model = model.into();
        self
    }

    /// These agents' web tools ask a server on this machine instead of the public engines and
    /// pages: `routes` names, per engine, the address it answers, and a read reaches any address.
    #[cfg(any(test, feature = "fixture-servers"))]
    #[must_use]
    pub fn web_looking_at(mut self, routes: Vec<(tilo_web_search::Engine, String)>) -> Self {
        self.web_routes = Some(routes);
        self
    }

    /// The web a turn of `workspace` reaches: the public engines and pages, or a test's server
    /// on this machine. A body that is not text is kept where the workspace's content is.
    #[allow(
        clippy::unused_self,
        reason = "the routes it reaches are a test's, and exist in no other build"
    )]
    fn web(&self, workspace: &str) -> web_search::Web {
        let content =
            ui::content::session(workspace).unwrap_or_else(ui::content::ContentStore::memory);
        let web = web_search::Web::new(content);
        #[cfg(any(test, feature = "fixture-servers"))]
        {
            if let Some(routes) = &self.web_routes {
                return web.looking_at(routes.clone());
            }
        }
        web
    }

    /// `agent` of `workspace` working for `person`: as its own approved key ([`agent_caller`]),
    /// or, for a sub-agent, as itself working under the approved key of the agent that heads
    /// its line of starters ([`Caller::working_under`]). A sub-agent holds no approval of its
    /// own: what it may do is what its starter's approval lets, and no more.
    ///
    /// # Errors
    /// As [`agent_caller`]: for a sub-agent, as its head's; and a workspace that keeps no list
    /// of its agents.
    async fn agent_caller(
        &self,
        workspace: WorkspaceId,
        agent: &Agent,
        person: [u8; 32],
    ) -> Result<Caller, String> {
        // A sub-agent acts under the approval of the agent that heads its line.
        if agent.sub.is_none() {
            return agent_caller(&self.roster, workspace, agent, person).await;
        }
        let record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let roster = self
            .read::<WorkspaceAgents>(&workspace.to_string(), AGENTS_OWNER, &record)
            .await?
            .ok_or("the workspace keeps no list of its agents")?;
        // A line whose head is gone ends at the sub-agent itself, which holds no approval:
        // nothing is done in its name then.
        let approved = roster.root_of(agent);
        Ok(agent_caller(&self.roster, workspace, approved, person)
            .await?
            .working_under(agent.id.0.clone(), &agent.name))
    }

    /// These agents, told the devices this server placed itself for its workspaces' windows:
    /// none of them is a device a person's assistant reads as in another workspace of
    /// theirs. Agents told none cannot tell such a device from a person's own, and read in
    /// no other workspace.
    #[must_use]
    pub fn knowing_placed(mut self, placed: crate::environment::EnvironmentRegistry) -> Self {
        self.placed = Some(placed);
        self
    }

    /// These agents, whose turns reach `initiative`: an agent reads its person's look-round and
    /// turns it on and off through the server's own actions.
    #[must_use]
    pub fn with_initiative(mut self, initiative: Option<Initiative>) -> Self {
        self.initiative = initiative;
        self
    }

    /// These agents, whose turns reach the Browser windows `browsers` places: a turn's open of
    /// Browser has its window placed in the turn's workspace, and a turn has a field of the
    /// page in its workspace's window filled from its person's vault by reference. With `None`
    /// such an open is refused in words that say this server has no machine for a window.
    #[must_use]
    pub fn placing(mut self, browsers: Option<Arc<dyn windows::Windows>>) -> Self {
        self.browsers = browsers;
        self
    }

    /// These agents, whose turns reach `events`: an agent sets its person's scheduled work and
    /// events up, pauses, changes and removes them through the host's own actions.
    #[must_use]
    pub fn with_events(mut self, events: Option<WorkspaceEvents>) -> Self {
        self.events = events;
        self
    }

    /// What lets a stopping server end these agents' work cleanly ([`Draining::finish`]).
    #[must_use]
    pub fn draining(&self) -> Draining {
        Draining(Arc::clone(&self.going))
    }

    /// Take every turn the agents' triggers ask of them ([`triggered`]) until the host stops,
    /// and keep the list of agents whose answer a trigger reaches as their workspaces' records
    /// say.
    pub async fn run(self) {
        let Some(mut asked) = self.answers.lock_recover().take() else {
            return;
        };
        let mut writes = self.workspaces.document_writes();
        let mut known = None;
        self.read_answering(&mut known).await;
        loop {
            tokio::select! {
                answer = asked.recv() => {
                    let Some(Answer { workspace, agent, event, saver, started }) = answer else {
                        return;
                    };
                    let agents = self.clone();
                    tokio::spawn(async move {
                        let taken = Box::pin(agents.triggered(&workspace, &agent, event, saver)).await;
                        started.complete(taken);
                    });
                }
                write = writes.recv() => match write {
                    Ok(write) if write.app != AGENTS_OWNER => {}
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        self.read_answering(&mut known).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
            }
        }
    }

    /// Read again which agents the workspaces here keep: each one's answer is what a trigger
    /// reaches ([`EngineKeys::answering`]). `known` holds the agents each workspace kept at the
    /// last read, `None` before the first: an agent this device's person made since comes with
    /// its triggers, saved as theirs ([`triggered::triggers`]), and one retired or removed
    /// since takes its triggers with it.
    async fn read_answering(&self, known: &mut Option<BTreeMap<String, BTreeSet<String>>>) {
        let record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let me = self.workspaces.serving_certificate().user_public_key;
        let mut answering = Vec::new();
        let mut now = BTreeMap::new();
        for workspace in self.workspaces.described().await {
            let id = workspace.id().to_string();
            let Ok(Some(agents)) = self
                .read::<WorkspaceAgents>(&id, AGENTS_OWNER, &record)
                .await
            else {
                continue;
            };
            let answers = |agent: &&Agent| {
                agent.state == tilo_workspace_agent::AgentState::Active
                    && agent.mcp_client.is_none()
                    && agent.sub.is_none()
            };
            let working: BTreeSet<String> = agents
                .agents
                .iter()
                .filter(answers)
                .map(|agent| agent.id.0.clone())
                .collect();
            if let Some(before) = known.as_ref().and_then(|known| known.get(&id)) {
                let saved: BTreeSet<String> = workspace
                    .shortcuts
                    .iter()
                    .map(|saved| saved.id().to_owned())
                    .collect();
                let mine = |agent: &Agent| {
                    tilo_action_policy::PersonId::parse(&agent.acts_for)
                        .is_ok_and(|person| person.root() == Some(me))
                };
                for agent in agents.agents.iter().filter(answers) {
                    let shortcut = triggered::shortcut_id(&agent.id.0);
                    if before.contains(&agent.id.0) || !mine(agent) || saved.contains(&shortcut) {
                        continue;
                    }
                    let save = tilo_automation::ShortcutChange::Save(Box::new(
                        triggered::triggers(&agent.id.0, &agent.name),
                    ));
                    if let Err(error) = self.workspaces.change_own_shortcuts(&id, save).await {
                        tracing::warn!(workspace = %id, agent = %agent.id.0, %error, "a new agent's triggers were not saved");
                    }
                }
                for gone in before.difference(&working) {
                    let shortcut = triggered::shortcut_id(gone);
                    if !saved.contains(&shortcut) {
                        continue;
                    }
                    let delete = tilo_automation::ShortcutChange::Delete(shortcut);
                    if let Err(error) = self.workspaces.change_own_shortcuts(&id, delete).await {
                        tracing::warn!(workspace = %id, agent = %gone, %error, "a retired agent's triggers were not removed");
                    }
                }
            }
            answering.extend(
                agents
                    .agents
                    .iter()
                    .filter(answers)
                    .map(|agent| (agent.id.0.clone(), agent.name.clone())),
            );
            now.insert(id, working);
        }
        *known = Some(now);
        answering.sort();
        answering.dedup_by(|a, b| a.0 == b.0);
        *self.answering.lock_recover() = answering;
    }

    /// Start `agent`'s turn in `workspace` on `event`, the change a trigger `saver` keeps fired
    /// on ([`triggered`]): whether this device took it. The turn is named by the agent and the
    /// item the change made, so every device that saw it names one turn and one device takes it
    /// ([`crate::work::takes_turn`]). The person it is for is whoever made the change; for a
    /// change made for nobody who is a member (a message a service carried in from outside),
    /// the person whose trigger fired, so the turn may do no more than they may.
    ///
    /// # Errors
    /// The change is nothing an agent answers, or the agent is none of the workspace's.
    pub async fn triggered(
        &self,
        workspace: &str,
        agent: &str,
        event: tilo_automation::dispatch::DocumentEvent,
        saver: String,
    ) -> Result<crate::work::Started, String> {
        let place = triggered::Where::of(&event)
            .ok_or("the change is no message or task an agent answers")?;
        let record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let roster = self
            .read::<WorkspaceAgents>(workspace, AGENTS_OWNER, &record)
            .await?
            .ok_or("the workspace keeps no list of its agents")?;
        let agent = roster
            .agents
            .iter()
            .find(|its| its.id.0 == agent)
            .cloned()
            .ok_or_else(|| format!("the workspace has no agent `{agent}`"))?;
        let request = triggered::said(&event);
        let turn = Turn {
            agent,
            person: triggered::asker(&event).unwrap_or(saver),
            conversation: format!("{}/{}", place.app, place.record),
            cause: event.change.clone(),
            asked: request.chars().take(120).collect(),
            request,
            ask: Ask::Triggered(place.clone()),
            history: Vec::new(),
            fold: None,
            typed: true,
            line: None,
        };
        let ask = triggered::answer_at(&turn.agent.id.0, &place);
        let Some(claim) = self.claim(workspace, &turn).await else {
            return Ok(crate::work::Started::Elsewhere(
                "another device takes this turn, or took it".to_owned(),
            ));
        };
        let going = match self.going.enter(workspace, &turn) {
            going::Entered::Began(going) => going,
            going::Entered::Goes(going) => {
                // What was written while the agent works there reaches the turn that goes.
                let _ = going.reach_read(turn, self.going.reading());
                return Ok(crate::work::Started::Taken);
            }
            going::Entered::Closed => return Err("this device's agents are stopping".to_owned()),
        };
        let agents = self.clone();
        let workspace = workspace.to_owned();
        tokio::spawn(async move {
            let (_, ended, summary) =
                Box::pin(agents.answer_going(&workspace, &turn, &going, Arc::clone(&claim))).await;
            if let Err(error) = &ended {
                tracing::warn!(%workspace, agent = %turn.agent.id.0, %error, "an agent's turn did not finish");
            }
            drop(going);
            let done = json!({"answered": ended.is_ok()});
            if let Err(error) =
                crate::work::keep_done(&agents.workspaces, &workspace, &ask, done).await
            {
                tracing::warn!(%workspace, %error, "an agent's turn is not kept as done");
            }
            if let Some(summary) = summary {
                Box::pin(agents.keep_summary(summary)).await;
            }
        });
        Ok(crate::work::Started::Taken)
    }

    /// Start every turn `workspace` owes its host's agents that is not going yet.
    ///
    /// # Errors
    /// The workspace's records could not be read.
    pub async fn look(&self, workspace: &str) -> Result<(), String> {
        // Boxed, here and in `turns` and `answer`: whoever awaits a round or a turn holds a
        // pointer to it and carries none of it in a future of its own.
        Box::pin(self.look_past(workspace, None)).await
    }

    /// [`Self::look`], leaving out the ask a turn of `past`'s agent just ended on. A turn that
    /// ended and is owed still changed nothing in the records (its answer could not be
    /// written): the next write brings it up again, as ever, and it is never started over at
    /// once, which would ask its person's model again and again.
    async fn look_past(&self, workspace: &str, past: Option<&(String, Ask)>) -> Result<(), String> {
        // What was read before a turn began or went on is not handed to that turn: the look
        // reads again. A few times at most, since a turn that moves writes, and its write
        // starts a look of its own.
        for _ in 0..READINGS {
            if Box::pin(self.look_reading(workspace, past)).await? {
                break;
            }
        }
        Ok(())
    }

    /// One reading of [`Self::look_past`]: whether what it read was still current wherever it
    /// was handed to a turn that goes.
    async fn look_reading(
        &self,
        workspace: &str,
        past: Option<&(String, Ask)>,
    ) -> Result<bool, String> {
        let read = self.read_look(workspace).await?;
        Ok(self.enter_past(workspace, read, past).await)
    }

    /// The first step of a look: what `workspace` owes, read once what its people answered is
    /// settled. [`Self::enter_look`] acts on it. A look takes
    /// both steps at once ([`Self::look`]); whoever must stand between the two, as a test of
    /// what a look does with a reading that a turn has overtaken, takes them one at a time.
    ///
    /// # Errors
    /// What the workspace owes could not be read.
    pub async fn read_look(&self, workspace: &str) -> Result<LookRead, String> {
        // What its people answered about what its agents asked them first
        // (general asks, vault uses, calendar passwords and mail passwords).
        Box::pin(self.settle_asks(workspace)).await;
        // Taken before anything is read: a turn that begins or goes on from here may answer
        // something newer than what this look reads.
        let reading = self.going.reading();
        let asks = self.asks(workspace).await?;
        Ok(LookRead { reading, asks })
    }

    /// The second step of a look: begin every turn `read` holds whose place is free, and hand
    /// the others to the turns that go there. Whether what was read was still current wherever
    /// it was handed on: where it was not, a look reads again.
    pub async fn enter_look(&self, workspace: &str, read: LookRead) -> bool {
        self.enter_past(workspace, read, None).await
    }

    /// [`Self::enter_look`], leaving out the ask `past` names ([`Self::look_past`]).
    async fn enter_past(
        &self,
        workspace: &str,
        LookRead { reading, asks }: LookRead,
        past: Option<&(String, Ask)>,
    ) -> bool {
        // An answer wakes whoever asked: written once under the question's own key.
        self.wake_answered(workspace, asks.answered);
        let is_past = |turn: &Turn| {
            past.is_some_and(|(agent, ask)| *agent == turn.agent.id.0 && *ask == turn.ask)
        };
        // A stop reaches the turn it stops. One no turn of this host is on (the host restarted
        // since) is ended in the record, so its reply says it was stopped.
        for turn in asks.stopped {
            if is_past(&turn) {
                continue;
            }
            match self.going.enter(workspace, &turn) {
                going::Entered::Goes(going) => {
                    if going.ask() == turn.ask {
                        going.stop();
                    }
                }
                // The next start ends it.
                going::Entered::Closed => {}
                going::Entered::Began(held) => {
                    let agents = self.clone();
                    let workspace = workspace.to_owned();
                    tokio::spawn(async move {
                        // The device whose turn it is ends it.
                        if agents.claim(&workspace, &turn).await.is_none() {
                            drop(held);
                            return;
                        }
                        let ended = (turn.agent.id.0.clone(), turn.ask.clone());
                        if let Err(error) = agents.end(&workspace, &turn, &Outcome::Cancelled).await
                        {
                            tracing::warn!(%workspace, agent = %ended.0, %error, "a stopped turn of a host agent was not ended");
                        }
                        agents.take_back_asked(&workspace, &turn, &turn.ask).await;
                        drop(held);
                        agents.look_after(workspace, ended).await;
                    });
                }
            }
        }
        let mut current = true;
        for turn in asks.turns {
            if is_past(&turn) {
                continue;
            }
            match self.going.enter(workspace, &turn) {
                // One turn at a time for an agent in a conversation: what its person wrote
                // since reaches the turn that is going, at its next step. Not what was read
                // before that turn began or last went on: it may be older than what the turn
                // answers now, and the turn would leave its person's newer words for it.
                going::Entered::Goes(going) => {
                    if going.reach_read(turn, reading) == going::Handed::Stale {
                        current = false;
                    }
                }
                // The server is stopping: the next start answers it.
                going::Entered::Closed => {}
                going::Entered::Began(held) => {
                    let agents = self.clone();
                    let workspace = workspace.to_owned();
                    tokio::spawn(async move {
                        // Another device whose turn it is answers it, or none that may.
                        let Some(claim) = agents.claim(&workspace, &turn).await else {
                            drop(held);
                            return;
                        };
                        let agent = turn.agent.id.0.clone();
                        // Boxed, as in `answer`: the task holds a pointer to the turn and
                        // carries none of it in a future of its own.
                        let (ended_on, ended, summary) =
                            Box::pin(agents.answer_going(&workspace, &turn, &held, claim)).await;
                        if let Err(error) = ended {
                            tracing::warn!(%workspace, %agent, %error, "a host agent's turn did not finish");
                        }
                        drop(held);
                        match summary {
                            // The summary is written with the place given back: what waited
                            // for the place begins beside it, not after it.
                            Some(summary) => {
                                let after = agents.clone().look_after(workspace, (agent, ended_on));
                                tokio::spawn(after);
                                Box::pin(agents.keep_summary(summary)).await;
                            }
                            None => agents.look_after(workspace, (agent, ended_on)).await,
                        }
                    });
                }
            }
        }
        // A note is handed on as its person's own words are: with what this look read.
        let carried = self.carry(workspace, asks.sub, reading).await;
        current && carried
    }

    /// This device's claim on `turn` of `workspace`: none where another device takes it, or
    /// this one may not ([`crate::work::takes_turn`]). The claim is named after the agent and
    /// the place it was asked in: whatever is written there while the turn goes is answered on
    /// the device that has it, and a claim stays until it goes unwritten for a minute, so the
    /// device that answered in a conversation answers what is written there next.
    async fn claim(&self, workspace: &str, turn: &Turn) -> Option<Arc<crate::work::Claim>> {
        let agent = &turn.agent.id.0;
        let ask = match &turn.ask {
            // A triggered turn starts on the device whose trigger fired.
            Ask::Triggered(place) => triggered::started_here(&self.workspaces, agent, place),
            Ask::Message { part: record, .. } | Ask::Issue { record, .. } => {
                let app = if matches!(turn.ask, Ask::Message { .. }) {
                    CHAT
                } else {
                    TASKS
                };
                let (_, _, place) = going::place(workspace, turn);
                crate::work::Turn::once(
                    format!("agents.agent:{agent}#answer"),
                    json!({ "place": place }),
                )
                .acting_as(crate::work::Actor::Agent(agent.clone()))
                .about(app, record.clone())
            }
        };
        match crate::work::takes_turn(&self.workspaces, workspace, &ask).await {
            Ok(claim) => Some(Arc::new(claim)),
            Err(not_taken) => {
                tracing::debug!(%workspace, %agent, %not_taken, "this device does not take an agent's turn");
                None
            }
        }
    }

    /// Look at `workspace` once a turn ended on `past`: its place is free, so what waited for
    /// it to end is started now and not at the next write. Boxed, since a look starts the turns
    /// that call it.
    fn look_after(
        self,
        workspace: String,
        past: (String, Ask),
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(async move {
            if let Err(error) = Box::pin(self.look_past(&workspace, Some(&past))).await {
                tracing::debug!(%workspace, %error, "the host's agents did not look at a workspace");
            }
        })
    }

    /// The turns `workspace`'s records owe the agents its host runs.
    ///
    /// # Errors
    /// A record could not be read.
    pub async fn turns(&self, workspace: &str) -> Result<Vec<Turn>, String> {
        Ok(Box::pin(self.asks(workspace)).await?.turns)
    }

    /// [`Self::turns`] at `now_ms`: an issue whose wait was to end by then is owed a turn again.
    ///
    /// # Errors
    /// A document could not be read.
    pub async fn turns_at(&self, workspace: &str, now_ms: i64) -> Result<Vec<Turn>, String> {
        Ok(Box::pin(self.asks_at(workspace, now_ms)).await?.turns)
    }

    /// What `workspace`'s records ask of the agents its host runs: the turns they owe, and the
    /// turns a person stopped whose work is not ended yet.
    async fn asks(&self, workspace: &str) -> Result<Asks, String> {
        self.asks_at(workspace, tilo_clock::unix_now_ms_i64()).await
    }

    /// [`Self::asks`] at `now_ms`.
    async fn asks_at(&self, workspace: &str, now_ms: i64) -> Result<Asks, String> {
        let agents = self.agents(workspace).await?;
        if agents.is_empty() {
            self.workspaces.note_waiting(workspace, Vec::new());
            return Ok(Asks::default());
        }
        let routes = self
            .workspaces
            .document_routes(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut asks = self.chat_asks(workspace, &agents, &routes).await?;
        asks.turns.extend(
            self.issue_turns(workspace, &agents, &routes, now_ms)
                .await?,
        );
        Ok(asks)
    }

    /// What `workspace`'s conversations ask of `agents`: the turns they owe, and the turns a
    /// person stopped whose work is not ended yet.
    async fn chat_asks(
        &self,
        workspace: &str,
        agents: &BTreeMap<String, Agent>,
        routes: &[(String, String)],
    ) -> Result<Asks, String> {
        let mut asks = Asks::default();
        // The questions its agents asked that wait for their person, read
        // from the same parts.
        let mut waiting = Vec::new();
        // The answers that owe their askers a turn, and the wakes written already.
        let mut answered = Vec::new();
        let mut woken = std::collections::BTreeSet::new();
        let Some(chat) = self.read::<chat::chat::Chat>(workspace, CHAT, "").await? else {
            self.workspaces.note_waiting(workspace, waiting);
            return Ok(asks);
        };
        let mut team = self.team(workspace, agents).await?;
        for (app, document) in routes {
            if app != CHAT
                || collab_types::document::DocumentId::parse(document)
                    .is_none_or(|part| chat::parts::part_index(&part).is_none())
            {
                continue;
            }
            let Some(part) = self
                .read::<chat::parts::ChatMessages>(workspace, CHAT, document)
                .await?
            else {
                continue;
            };
            waiting.extend(self::asks::waiting_in(workspace, document, &part, agents));
            answered.extend(answers::answered_in(&part, agents));
            woken.extend(answers::woken_in(&part));
            for summons in chat::summons::summonses(&chat, &part) {
                let Some(agent) = agents.get(&summons.agent) else {
                    continue;
                };
                let Some(message) = part.messages.get(&summons.message) else {
                    continue;
                };
                let request = carried::request(message);
                // What came before it in the conversation, as every turn is told it: the
                // same on a host as on the device that would answer it.
                // What was learned about the person on their devices is their own record,
                // which nothing here reads. What this server keeps of what its own agents
                // learned is told to a turn of the person it is about, in their own
                // conversation with the agent ([`learned`]), and to no other.
                let asked_by = (summons.person.as_str(), &summons.conversation);
                let known =
                    learned::learned_for(&self.workspaces, workspace, agent, asked_by).await;
                let told = chat::recall::told(
                    &chat,
                    &part,
                    known.as_ref(),
                    &summons.conversation,
                    &summons.message,
                    &collaboration::permissions::MemberId::agent(&summons.agent),
                    request,
                );
                asks.turns.push(Turn {
                    agent: agent.clone(),
                    person: summons.person.clone(),
                    conversation: summons.address(),
                    cause: summons.message.to_string(),
                    request: told.request,
                    history: told.history,
                    asked: chat::summons::asked_words(message),
                    fold: told.fold,
                    typed: summons.typed,
                    line: None,
                    ask: Ask::Message {
                        part: document.clone(),
                        message: summons.message.to_string(),
                        attempt: summons.attempt,
                    },
                });
            }
            for summons in chat::summons::stopped(&chat, &part) {
                let Some(agent) = agents.get(&summons.agent) else {
                    continue;
                };
                asks.stopped.push(Turn {
                    agent: agent.clone(),
                    person: summons.person.clone(),
                    conversation: summons.address(),
                    cause: summons.message.to_string(),
                    request: String::new(),
                    history: Vec::new(),
                    asked: String::new(),
                    fold: None,
                    typed: summons.typed,
                    line: None,
                    ask: Ask::Message {
                        part: document.clone(),
                        message: summons.message.to_string(),
                        attempt: summons.attempt,
                    },
                });
            }
            team.read(document, &chat, &part);
        }
        self.workspaces.note_waiting(workspace, waiting);
        answered.retain(|wake| !woken.contains(&wake.key));
        asks.answered = answered;
        team.settle(&mut asks);
        Ok(asks)
    }

    /// The turns `workspace`'s issues owe `agents` at `now_ms`: an issue that waits is owed one
    /// again once the latest moment its wait ends has come.
    async fn issue_turns(
        &self,
        workspace: &str,
        agents: &BTreeMap<String, Agent>,
        routes: &[(String, String)],
        now_ms: i64,
    ) -> Result<Vec<Turn>, String> {
        // Every part is taken before anything is asked: an issue's pieces may be kept in another
        // part than the issue itself.
        let mut kept = Vec::new();
        for (app, record) in routes {
            if app != TASKS
                || collab_types::document::DocumentId::parse(record)
                    .and_then(|id| id.chain_part())
                    .is_none_or(|(chain, _)| chain != tasks::issue_list::chain())
            {
                continue;
            }
            let bytes = self
                .workspaces
                .resource_document(workspace, TASKS, record)
                .await
                .map_err(|error| error.to_string())?;
            if let Some(bytes) = bytes {
                kept.push((record, bytes));
            }
        }
        // They are read only now that nothing more is awaited: an issue's rich text does not
        // travel between threads, and this look does.
        let mut parts = Vec::new();
        for (document, bytes) in kept {
            let mut doc = automerge::AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
            let part = crate::agents::opened::<tasks::issue_list::IssueList>(&mut doc)?;
            parts.push((document, part));
        }
        let asks = tasks_model::agent_asks_at(
            parts.iter().flat_map(|(_, part)| part.issues.iter()),
            now_ms,
        );
        let mut turns = Vec::new();
        for ask in asks {
            let Some(agent) = agents.get(&ask.agent) else {
                continue;
            };
            let Some((document, issue)) = parts.iter().find_map(|(document, part)| {
                part.issues.get(&ask.issue).map(|issue| (*document, issue))
            }) else {
                continue;
            };
            let request = issue_work::asked(issue, &ask);
            turns.push(Turn {
                agent: agent.clone(),
                person: ask.person.person_id().unwrap_or_default().to_owned(),
                conversation: tasks_model::issue_resource(&ask.issue),
                cause: ask.cause.clone(),
                request,
                history: Vec::new(),
                asked: issue.title.clone(),
                fold: None,
                typed: true,
                line: None,
                ask: Ask::Issue {
                    record: document.clone(),
                    issue: ask.issue.to_string(),
                    cause: ask.cause,
                },
            });
        }
        Ok(turns)
    }

    /// The agents of `workspace` its host runs, by id.
    async fn agents(&self, workspace: &str) -> Result<BTreeMap<String, Agent>, String> {
        let record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let Some(agents) = self
            .read::<WorkspaceAgents>(workspace, AGENTS_OWNER, &record)
            .await?
        else {
            return Ok(BTreeMap::new());
        };
        // An agent is the workspace's: it works here for whichever member asks it, whoever
        // made it, and each turn is held to what the person who asked may do now.
        Ok(agents
            .agents
            .iter()
            .filter(|agent| agent.is_active() && agent.mcp_client.is_none())
            .map(|agent| (agent.id.0.clone(), agent.clone()))
            .collect())
    }

    async fn read<R: tilo_resource::Resource>(
        &self,
        workspace: &str,
        app: &str,
        document: &str,
    ) -> Result<Option<R::State>, String> {
        self.workspaces
            .read_resource::<R>(workspace, (app, document))
            .await
    }

    /// Answer `turn` in `workspace`: tell the person it is taken, work through the agent's tools
    /// until the model has its answer, and give the answer where the ask was made. A turn of
    /// the same agent that goes in the same conversation already is left to go on, and a
    /// message that is owed no answer any more is not begun ([`Self::owed_now`]). A turn that
    /// is another device's is left to it ([`Self::claim`]).
    ///
    /// # Errors
    /// The person is not admitted here on any device, or the answer could not be written.
    pub async fn answer(&self, workspace: &str, turn: &Turn) -> Result<(), String> {
        match self.going.enter(workspace, turn) {
            going::Entered::Began(held) => {
                let Some(claim) = self.claim(workspace, turn).await else {
                    return Ok(());
                };
                let (_, ended, summary) =
                    Box::pin(self.answer_going(workspace, turn, &held, claim)).await;
                // The summary it still owes is written with the place given back.
                drop(held);
                if let Some(summary) = summary {
                    Box::pin(self.keep_summary(summary)).await;
                }
                ended
            }
            going::Entered::Goes(_) | going::Entered::Closed => Ok(()),
        }
    }

    /// Whom `turn`'s agent acts for in `workspace`, and through what.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the turn's own providers are handed to its service, which keeps them for the turn"
    )]
    async fn scope(&self, workspace: &str, turn: &Turn) -> Result<TurnScope, String> {
        let workspace_id = WorkspaceId::parse(workspace).ok_or("not a workspace id")?;
        let person: [u8; 32] = hex::decode(&turn.person)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or("the ask names no person")?;
        // A host serves only a person it admits on a device of theirs; on the person's own
        // device the person is the one whose root made the agent's key ([`agent_caller`]).
        if matches!(self.roster, Roster::Hosted(..))
            && self
                .workspaces
                .person_device(workspace_id, person)
                .await
                .is_none()
        {
            return Err("the person who asked is admitted here on no device".to_owned());
        }
        let agent = self.agent_caller(workspace_id, &turn.agent, person).await?;
        let mut scope = TurnScope {
            provider: WorkspaceActionProvider::new(self.actions.clone(), workspace_id)
                .offering(self.events.clone())
                .offering_initiative(self.initiative.clone())
                .serving_ask(&turn.request),
            agent,
            person,
            workspace: workspace.to_owned(),
            workspace_id,
        };
        team::join(&self.workspaces, turn, &mut scope).await;
        across::join(&self.workspaces, self.placed.as_ref(), turn, &mut scope).await;
        // What the turn's tools reach beside the workspace's resources, each a provider of the
        // turn's own ([`TurnProvider`]): what its person's vault lets it use by reference.
        // Beside that, the services its person connected, by this server's own entry for a turn.
        let own = connections::of(self.as_asker(), turn, &scope.reply(turn), workspace_id).await;
        scope.provider = scope.provider.answering(own);
        let uses = vault_uses::TurnUses::for_turn(self, turn, &scope.reply(turn)).await;
        let mail = mail::TurnMail::for_turn(self, turn, &scope.reply(turn)).await;
        // What its person's calendars hold, to read, under the bound their mail is under.
        let calendars =
            calendars::TurnCalendars::for_turn(&self.workspaces, turn, &scope.reply(turn)).await;
        scope.provider = scope
            .provider
            .answering(uses)
            .answering(mail)
            .answering(calendars);
        // What connects a calendar its person tells it of: they are asked for its password
        // where no agent reads it.
        let connects =
            calendar_connect::TurnConnects::for_turn(self, turn, &scope.reply(turn)).await;
        scope.provider = scope.provider.answering(connects);
        // What signs a mail account in from its address alone: the same asking, for mail.
        let signs_in =
            mail_connect::TurnMailConnects::for_turn(self, turn, &scope.reply(turn)).await;
        scope.provider = scope.provider.answering(signs_in);
        // Its open of Browser, whose window this server places where it has a machine for one.
        let browser =
            turn_browser::TurnBrowser::for_turn(self.browsers.clone(), &scope.reply(turn));
        scope.provider = scope.provider.answering(browser);
        // What it does at that window's page: where it has the window sent.
        let pages = turn_pages::TurnPages::for_turn(
            self.browsers.clone(),
            &self.workspaces,
            turn,
            &scope.reply(turn),
        )
        .await;
        scope.provider = scope.provider.answering(pages);
        Ok(scope)
    }

    /// End `turn` as `outcome` without working on it: its reply says how it ended.
    async fn end(&self, workspace: &str, turn: &Turn, outcome: &Outcome) -> Result<(), String> {
        let acting = self.scope(workspace, turn).await?;
        let reply = acting.reply(turn);
        // An answer that went on past one message ends in each of its messages.
        if let Ask::Message { part, .. } = &turn.ask
            && let Ok(Some(held)) = self
                .read::<chat::parts::ChatMessages>(workspace, CHAT, part)
                .await
        {
            reply.written.follows_from(&held);
        }
        let ended = reply.finished(outcome).await;
        self.sub_ended(&acting, turn, outcome, false).await;
        ended
    }

    /// Whether a host that stopped left `turn` begun: its reply shows a step taken, and no turn
    /// of this host is on it, since this one only begins now. Words that turn's model wrote
    /// into the reply stay there, and `reply` goes on below them ([`written`]).
    async fn left_begun(&self, workspace: &str, turn: &Turn, reply: &Reply) -> bool {
        let Ask::Message {
            part,
            message,
            attempt,
        } = &turn.ask
        else {
            return false;
        };
        match self
            .read::<chat::parts::ChatMessages>(workspace, CHAT, part)
            .await
        {
            Ok(Some(held)) => {
                reply.written.goes_on_from(&held);
                chat::summons::left_begun(&held, message, &turn.agent.id.0, *attempt)
            }
            _ => false,
        }
    }

    /// `turn` as `workspace` gives it now that its place is held, and none where it is owed no
    /// longer. Whoever gave the turn read the workspace earlier, and a turn that held the place
    /// then may have answered the message since, or its person stopped it, asked it again or
    /// wrote something newer. Begun as it was read, it would ask its model a second time and
    /// set a finished reply back to taken. What is owed instead begins at the next look, and a
    /// turn that is still owed is told its conversation as it stands now, with the answer
    /// before it.
    ///
    /// An issue's turn is held to the same: where its agent answered the ask since, or the
    /// issue waits or stopped since, nothing begins, so no second session is counted on it and
    /// no model is asked again. Whether an issue is owed depends on the time as well, and
    /// whoever read the turn may have read it at a later time than the clock's
    /// ([`Self::turns_at`]). The one ask that hangs on the time is a wait that ended, which
    /// names its moment and is owed from then on: so what the clock's time does not owe is
    /// looked for once more with every wait ended, where only that same ask is found.
    ///
    /// # Errors
    /// What the workspace owes could not be read.
    async fn owed_now(&self, workspace: &str, turn: &Turn) -> Result<Option<Turn>, String> {
        // A triggered turn is owed by the change that fired it, and by nothing read here.
        if matches!(turn.ask, Ask::Triggered(_)) {
            return Ok(Some(turn.clone()));
        }
        let same = |now: &Turn| now.agent.id.0 == turn.agent.id.0 && now.ask == turn.ask;
        let owed = Box::pin(self.asks(workspace)).await?.turns;
        if let Some(owed) = owed.into_iter().find(same) {
            return Ok(Some(owed));
        }
        if matches!(turn.ask, Ask::Message { .. }) {
            return Ok(None);
        }
        let owed = Box::pin(self.asks_at(workspace, i64::MAX)).await?.turns;
        Ok(owed.into_iter().find(same))
    }

    /// [`Self::answer`], for the turn that holds its place as `going`: the ask it ended on,
    /// which is a later one when its person wrote again while it worked, and how it ended.
    /// With them the summary it still owes its conversation, which whoever holds the place
    /// writes once the place is given back ([`summary::Owed`]).
    async fn answer_going(
        &self,
        workspace: &str,
        turn: &Turn,
        going: &going::Going,
        claim: Arc<crate::work::Claim>,
    ) -> (Ask, Result<(), String>, Option<summary::Owed>) {
        // The place is this turn's own from here on, and nothing else answers in it: what the
        // workspace owes now is what begins.
        let turn = match self.owed_now(workspace, turn).await {
            Ok(Some(owed)) => owed,
            Ok(None) => return (turn.ask.clone(), Ok(()), None),
            Err(error) => return (turn.ask.clone(), Err(error), None),
        };
        let scope = match self.scope(workspace, &turn).await {
            Ok(scope) => scope,
            Err(error) => return (turn.ask.clone(), Err(error), None),
        };
        let began = turn.ask.clone();
        let mut reply = scope.reply(&turn);
        reply.claim = Some((claim, going.stop_flag()));
        if self.left_begun(workspace, &turn, &reply).await {
            // Its steps may have changed things already, and nothing says which: starting over
            // would do them twice. It ends here, and asking again is the person's to do.
            let left = Outcome::Failed(LEFT_BEGUN.to_owned());
            let ended = reply.finished(&left).await;
            crate::agents::telling::turn_ended(
                &self.workspaces,
                self.events.as_ref(),
                &scope.person,
                workspace,
                &turn,
                &left,
            )
            .await;
            self.sub_ended(&scope, &turn, &left, false).await;
            return (turn.ask, ended, None);
        }
        Box::pin(self.answer_by_model(workspace, (scope, turn, reply, began), going)).await
    }

    /// The rest of [`Self::answer_going`] for a turn that the agent's model works: its scope,
    /// the turn, its reply and the ask it began with, as `answer_going` made them.
    async fn answer_by_model(
        &self,
        workspace: &str,
        (scope, mut turn, mut reply, began): (TurnScope, Turn, Reply, Ask),
        going: &going::Going,
    ) -> (Ask, Result<(), String>, Option<summary::Owed>) {
        // An issue is worked on over many agent sessions: its place is made and the session
        // counted before the model is asked, and one that may not begin ends here with its
        // plain line.
        let task = match self.issue_session(workspace, &mut turn, &reply).await {
            Ok(task) => task,
            Err(words) => {
                let ended = reply.finished(&Outcome::Stopped(words)).await;
                return (turn.ask, ended, None);
            }
        };
        // A task of Tasks is worked the same way: Tasks says whether its session may begin, and
        // one it refuses ends here with Tasks' words, said on the task.
        let tl_task = match task_session(&mut turn, &reply).await {
            Ok(task) => task,
            Err(words) => {
                let ended = reply.finished(&Outcome::Stopped(words)).await;
                return (turn.ask, ended, None);
            }
        };
        let clock = tilo_clock::now();
        reply.taken().await;
        self.sub_began(&scope, &turn).await;
        let from_starter = turn.line.as_ref().is_some_and(|line| line.from_starter);
        let day = today();
        // A sub-agent spends on the day of the person's own agent it works under.
        let limit = sub_agents::payer(&turn).spend_limit_micros;
        let spent_before = sub_agents::payer(&turn).spent_on(&day);
        let asked = self
            .model_for_turn(
                (workspace, scope.workspace_id),
                &turn,
                &day,
                going.stop_flag(),
            )
            .await;
        let keeping = learned::Keeping::of(&self.workspaces, workspace, &turn);
        let (outcome, cost, usage) = match (asked, limit) {
            (_, Some(limit)) if spent_before >= limit => (
                Outcome::Stopped(format!(
                    "I stopped before starting: today's limit of {} is reached.",
                    money(limit)
                )),
                0,
                TurnUsage::default(),
            ),
            (Err(why), _) => (Outcome::Failed(why), 0, TurnUsage::default()),
            (Ok(model), _) => {
                // The day's limit and the issue's own budget both bound the turn: the smaller.
                let day_left = limit.map(|limit| limit.saturating_sub(spent_before));
                let task_left = match (&task, &tl_task) {
                    (Some((_, before)), _) => before.money_left(),
                    (None, Some((_, before))) => before.money_left,
                    (None, None) => None,
                };
                let budget = match (day_left, task_left) {
                    (Some(day), Some(task)) => Some(day.min(task)),
                    (day, task) => day.or(task),
                };
                let keeping = keeping.as_ref();
                let web = self.web(workspace);
                Box::pin(work(
                    model, &scope, &mut turn, budget, &mut reply, going, keeping, web,
                ))
                .await
            }
        };
        let outcome = match (&task, &tl_task) {
            (Some((task, before)), _) => {
                task.ended_as(outcome, cost, clock.elapsed(), before).await
            }
            (None, Some((task, before))) => {
                task.ended_as(outcome, cost, clock.elapsed(), before).await
            }
            (None, None) => outcome,
        };
        // An answer is logged where the model gives it; a turn that ends any other way says
        // so here, or the log reads as if it were still going.
        match &outcome {
            Outcome::Answered(_) => {}
            Outcome::Stopped(why) => {
                tracing::info!(agent = %turn.agent.id.0, cost, %why, "an agent on the workspace's server stopped");
            }
            Outcome::Failed(why) => {
                tracing::warn!(agent = %turn.agent.id.0, cost, %why, "an agent on the workspace's server could not go on");
            }
            Outcome::Cancelled => {
                tracing::info!(agent = %turn.agent.id.0, cost, "an agent on the workspace's server was stopped by its person");
            }
        }
        if cost > 0 {
            self.charge(workspace, sub_agents::payer(&turn), &day, cost)
                .await;
        }
        let ended = reply.finished_with(&outcome, cost, &usage).await;
        crate::agents::telling::turn_ended(
            &self.workspaces,
            self.events.as_ref(),
            &scope.person,
            workspace,
            &turn,
            &outcome,
        )
        .await;
        if matches!(outcome, Outcome::Cancelled) {
            // Its person stopped it: what it asked them and still waits is taken back.
            self.take_back_asked(workspace, &turn, &began).await;
        }
        self.sub_ended(&scope, &turn, &outcome, from_starter).await;
        // The answer is finished for its person, its words being whole. What it asks into its
        // conversation's summary takes a model's time: it is written once this turn's place
        // is given back, so nothing its person writes meanwhile waits for it.
        let ended_on = turn.ask.clone();
        let summary = matches!(outcome, Outcome::Answered(_))
            .then(|| summary::Owed::of((workspace, scope.workspace_id), turn, reply, day))
            .flatten();
        (ended_on, ended, summary)
    }

    /// Begin an agent session on the issue `turn` asks about: its place is made when it has
    /// none, the session is said to have begun, and `turn` is told where the issue's working
    /// material is and what its note reads now. The issue's work as it read before the session;
    /// none for a turn that is no issue's, or whose issue is gone.
    ///
    /// # Errors
    /// No session may begin (its budget is used up, or its sessions were lost too often): the
    /// plain line, which is written on the issue as its stop.
    async fn issue_session(
        &self,
        workspace: &str,
        turn: &mut Turn,
        reply: &Reply,
    ) -> Result<Option<(issue_work::IssueWork, tasks_model::Work)>, String> {
        let (document, issue) = match &turn.ask {
            Ask::Issue {
                record: document,
                issue,
                ..
            } => (document.clone(), issue.clone()),
            Ask::Message { .. } | Ask::Triggered(_) => return Ok(None),
        };
        let id = tilo_resource::Id::from_raw(issue.clone());
        // Only what a session needs of the issue is taken from its document: an issue's rich
        // text does not travel between threads, and this turn does.
        let kept = self
            .read::<tasks::issue_list::IssueList>(workspace, TASKS, &document)
            .await
            .ok()
            .flatten()
            .and_then(|part| part.issues.get(&id).map(issue_work::Kept::of));
        let Some(kept) = kept else {
            return Ok(None);
        };
        let task = issue_work::IssueWork {
            service: reply.provider.clone(),
            caller: reply.caller.clone(),
            workspace: workspace.to_owned(),
            document,
            issue,
        };
        if let Some((why, words)) = issue_work::refused(kept.work.as_ref()) {
            task.stopped(why, &words).await;
            return Err(words);
        }
        let place = task.place(&self.workspaces, &kept).await;
        if let Err(error) = task.began().await {
            tracing::warn!(%workspace, issue = %task.issue, %error, "a session on an issue is not said to have begun");
        }
        let before = kept.work.unwrap_or_default();
        turn.request = issue_work::with_place(&turn.request, &before, &place);
        Ok(Some((task, before)))
    }

    /// The model `turn` is worked with, or why none works on it, in words its person reads.
    ///
    /// The workspace pays, so its own model answers: the one it linked for work its agents do.
    /// Once it linked a provider its settings decide alone, and a choice that cannot be used is
    /// said, never replaced by a model somebody else pays for. A workspace that linked nothing is
    /// told what to link.
    async fn model_for(
        &self,
        place: (&str, WorkspaceId),
        turn: &Turn,
        day: &str,
    ) -> Result<Model, String> {
        self.model_for_turn(place, turn, day, Arc::default()).await
    }

    /// [`Self::model_for`], for a turn its person may stop: a call of the workspace's model that
    /// is under way ends once `stop` is set.
    async fn model_for_turn(
        &self,
        (workspace, workspace_id): (&str, WorkspaceId),
        turn: &Turn,
        day: &str,
        stop: Arc<AtomicBool>,
    ) -> Result<Model, String> {
        let theirs = {
            // Every call is counted on the agent's day as it is answered, so turns of
            // one agent going at once cannot each spend the day's whole limit.
            let named_model = turn.agent.model.as_deref().or_else(|| {
                turn.agent.sub.as_ref().and_then(|sub| {
                    crate::workspaces::person_model::Interaction::from_id(&sub.kind)
                        .is_none()
                        .then_some(sub.kind.as_str())
                })
            });
            let theirs = self
                .workspaces
                .workspace_model(workspace_id, sub_agents::kind_of(&turn.agent))
                // The model the agent itself names answers where the workspace linked it on
                // this server: a sub-agent's is its skill's, or the one its starter named.
                .naming(named_model)
                .spending_as(Spender {
                    workspace: workspace.to_owned(),
                    agent: sub_agents::payer(turn).id.0.clone(),
                    name: sub_agents::payer(turn).name.clone(),
                    day: day.to_owned(),
                    limit_micros: sub_agents::payer(turn).spend_limit_micros,
                });
            // A turn a look of its person's look-round started counts on the
            // look-round's own day as well, and stops where that day's limit is reached.
            let look_round = self.initiative.as_ref().and_then(|initiative| {
                initiative.day_of_turn(&workspace_id.to_string(), &turn.person, &turn.cause, day)
            });
            match look_round {
                Some(look_round) => theirs.spending_as(look_round),
                None => theirs,
            }
        };
        let asked = theirs.clone();
        let language = self.language();
        let linked = tokio::task::spawn_blocking(move || asked.linked())
            .await
            .map_err(|error| error.to_string())?
            .map(|linked| {
                let context_chars = linked.model.context_chars();
                (theirs, linked.model.id, context_chars)
            });
        match linked {
            Ok((theirs, name, context_chars)) => Ok(Arc::new(move || {
                Some(Box::new(WorkspaceTurns {
                    model: theirs.clone(),
                    name: name.clone(),
                    stop: Arc::clone(&stop),
                    answered: None,
                    context_chars,
                    language: language.clone(),
                }) as Box<dyn PlanTurns>)
            })),
            Err(refused @ ModelRefusal::NoModelLinked { .. }) => self
                .model
                .clone()
                .ok_or_else(|| refused.words(language.as_deref())),
            Err(refused) => Err(refused.words(language.as_deref())),
        }
    }

    /// Count `cost` against `agent`'s day ([`count_spend`]).
    async fn charge(&self, workspace: &str, agent: &Agent, day: &str, cost: u64) {
        if let Err(error) = count_spend(&self.workspaces, workspace, agent, day, cost).await {
            tracing::warn!(%workspace, agent = %agent.id.0, %error, "an agent's spend is not counted");
        }
    }
}

/// Count `cost` against `agent`'s day on the workspace's record of its agents, for its keeper:
/// a model the host asked for the agent charged it, and the keeper's limit is read from it. The
/// host writes it as the workspace's governor, the count its keeper's own device would write for
/// an agent it runs, and as a change of Chat's: a host decides it against Chat's surface
/// like any other. Whatever the host asks a keeper's model for on their agent's behalf counts
/// here: a turn's calls, and a judgement one of the agent's events asked.
///
/// # Errors
/// Why the workspace's record did not take the count.
pub async fn count_spend(
    workspaces: &Workspaces,
    workspace: &str,
    agent: &Agent,
    day: &str,
    cost: u64,
) -> Result<(), String> {
    let keeper = agent.acts_for.clone();
    let record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    let (id, day) = (agent.id.0.clone(), day.to_owned());
    workspaces
        .author_application(
            workspace,
            workspaces.governor(),
            &tilo_agent_session::AppCatalogue::native(),
            AGENTS_OWNER,
            &record,
            |doc| {
                let mut counting =
                    tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
                        .as_author(&keeper)
                        .in_app(AGENTS_OWNER, "Assistant");
                let applied = counting
                    .apply(WorkspaceAgents::RECORD_AGENT_SPEND.with(
                        tilo_workspace_agent::workspace_agents::RecordAgentSpend {
                            agent: tilo_resource::Id::from_raw(id),
                            day,
                            cost_micros: cost,
                        },
                    ))
                    .map(drop)
                    .map_err(|error| error.to_string());
                std::mem::swap(doc, counting.doc_mut());
                applied
            },
        )
        .await
        .map_err(|error| error.to_string())
}

/// A turn's model calls, asked of the model the workspace linked, with its own key.
struct WorkspaceTurns {
    model: PersonModel,
    /// The model's name, for the usage log.
    name: String,
    /// Set when the turn's person stopped it: a call under way ends on it.
    stop: Arc<AtomicBool>,
    /// What answered the last call, as its person named the provider.
    answered: Option<ModelAnswered>,
    /// The most characters of conversation this model reads, by its descriptor's context.
    context_chars: usize,
    /// The language a failure is said in ([`ServerAgents::speaking`]).
    language: Option<String>,
}

impl PlanTurns for WorkspaceTurns {
    fn turn(
        &mut self,
        request: &ModelRequest,
        key: &str,
        deadline: Instant,
        retry: bool,
    ) -> Result<ModelResponse, ModelError> {
        self.turn_streaming(request, key, deadline, retry, &mut |_| {})
    }

    fn turn_streaming(
        &mut self,
        request: &ModelRequest,
        _: &str,
        deadline: Instant,
        _: bool,
        written: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<ModelResponse, ModelError> {
        let answer = self
            .model
            .answer_blocking(
                request,
                deadline,
                &|| self.stop.load(Ordering::Relaxed),
                written,
            )
            .map_err(|refused| refused.model_error_in(self.language.as_deref()))?;
        self.answered = Some(ModelAnswered {
            provider: answer.linked.provider,
            label: answer.linked.model.display_label().to_owned(),
            model: answer.linked.model.id,
        });
        Ok(answer.response)
    }

    fn model(&self) -> &str {
        &self.name
    }

    fn answered_by(&self) -> Option<ModelAnswered> {
        self.answered.clone()
    }

    fn context_chars(&self) -> Option<usize> {
        Some(self.context_chars)
    }
}

/// What answered a turn's calls and what they used, as its reply keeps it.
#[derive(Clone, Debug, Default)]
struct TurnUsage {
    /// The provider and model of the last call, where a person's own model answered.
    answered: Option<ModelAnswered>,
    /// The tokens its calls counted: every input and output token.
    tokens: u64,
}

/// How a turn ended.
pub use tilo_agent_turn::Outcome;

/// Whom a turn's agent acts for in a workspace, and through what: the same for every ask the
/// turn goes on with.
struct TurnScope {
    provider: WorkspaceActionProvider,
    /// The agent as its own approved key, working for the person who asked
    /// ([`ServerAgents::agent_caller`]).
    agent: Caller,
    /// The person who asked.
    person: [u8; 32],
    workspace: String,
    workspace_id: WorkspaceId,
}

impl TurnScope {
    /// Where `turn`'s answer is written, as its agent acting for the person inside that turn.
    /// The turn is one agent session of its agent's, as a connected agent's work is: each ask
    /// its own, which History groups its changes under and Console lists as one.
    fn reply(&self, turn: &Turn) -> Reply {
        let session = format!(
            "agent-session:host-{}",
            blake3::hash(
                format!(
                    "{}\0{}\0{}\0{}",
                    self.workspace, turn.conversation, turn.agent.id.0, turn.cause
                )
                .as_bytes()
            )
            .to_hex()
            .get(..24)
            .unwrap_or_default()
        );
        let cause =
            tilo_action_policy::turn::Turn::new(turn.conversation.clone(), turn.cause.clone());
        let caller = self
            .agent
            .clone()
            .in_agent_session(&session)
            .asked(&turn.asked)
            .on_roster_turn(cause);
        let provider = self.provider.clone();
        Reply {
            provider,
            caller,
            workspace: self.workspace.clone(),
            ask: turn.ask.clone(),
            written: written::Written::of(turn),
            shows_in: shares::shows_in(turn),
            claim: None,
        }
    }
}

/// Where a turn's answer is written: the reply Chat derives for the message, or the agent's
/// comment on the issue.
struct Reply {
    provider: WorkspaceActionProvider,
    caller: Caller,
    workspace: String,
    ask: Ask,
    /// How far the reply has grown while its model wrote it.
    written: written::Written,
    /// The conversation the turn may show a document in, where it has one ([`shares::shows_in`]).
    shows_in: Option<String>,
    /// This device's claim on the turn, with the turn's stop: renewed with each write, and the
    /// turn stops once another device has it ([`Self::kept`]).
    claim: Option<(Arc<crate::work::Claim>, Arc<std::sync::atomic::AtomicBool>)>,
}

impl Reply {
    /// Whether the turn is still this device's, its claim pushed out again: a turn another
    /// device took is stopped and writes nothing more, so one device answers.
    ///
    /// # Errors
    /// Another device has the turn.
    async fn kept(&self) -> Result<(), String> {
        let Some((claim, stop)) = &self.claim else {
            return Ok(());
        };
        if claim.renew().await {
            return Ok(());
        }
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        Err("another device took this turn".to_owned())
    }

    /// Tell the person the turn is taken, where their ask shows it. The reply is there from
    /// then on, and takes what the turn's model writes while it writes ([`written`]).
    async fn taken(&self) {
        if let Ask::Message { .. } = &self.ask
            && self.work(json!({"state": "taken"}), None).await.is_ok()
        {
            self.written.open();
        }
    }

    /// Tell the person what the agent is doing now. A note Chat does not take costs the person
    /// the step alone, and [`Self::work`] says why.
    async fn doing(&self, did: &str) {
        let _ = match &self.ask {
            Ask::Message { .. } => self.work(json!({"state": "doing", "did": did}), None).await,
            Ask::Issue { .. } | Ask::Triggered(_) => self.kept().await,
        };
    }

    /// Tell the person what the agent is doing now, and wait for nothing: the note is handed
    /// to the workspace's action service, which takes a workspace's requests in the order
    /// they came. So it stands on the reply before whatever the turn writes there next, and
    /// no tool waits until Chat has taken it. Whether the turn has a reply that takes notes.
    fn notes(&self, did: &str) -> bool {
        let Ask::Message { part, message, .. } = &self.ask else {
            return false;
        };
        let route = DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: self.workspace.clone(),
            document: part.clone(),
        };
        let mut arguments = Map::new();
        arguments.insert("message".into(), json!(message));
        arguments.insert("work".into(), json!({"state": "doing", "did": did}));
        let resource = format!("{}/messages/{message}", chat::parts::ChatMessages::KIND);
        let request = invocation_of(
            target_named(&route, &resource, KEEP_WORK),
            InvokeArgument::Json(arguments),
        );
        let workspace = self.workspace.clone();
        self.provider
            .invoke(&self.caller, request)
            .then(move |receipt| {
                let refused = match receipt {
                    Some(Ok(receipt)) => match receipt.status {
                        ReceiptStatus::Failed { error } => Some(error.message),
                        ReceiptStatus::Rejected { error } => Some(error.to_string()),
                        ReceiptStatus::Accepted
                        | ReceiptStatus::Completed { .. }
                        | ReceiptStatus::Cancelled { .. } => None,
                    },
                    Some(Err(error)) => Some(error.to_string()),
                    None => Some("the workspace's action service let go of it".to_owned()),
                };
                if let Some(error) = refused {
                    tracing::warn!(%workspace, %error, "the reply of an agent on the workspace's server did not take its note");
                }
            });
        true
    }

    /// Leave this ask for what its person wrote next: its reply says the answer follows below.
    /// A reply that holds what its model wrote keeps that, and ends there.
    ///
    /// # Errors
    /// Chat did not keep that the reply ended ([`Self::unfinished`]).
    async fn went_on(&self) -> Result<(), String> {
        if self.grown_ends(&Outcome::Answered(String::new())).await {
            return Ok(());
        }
        match self
            .work(json!({"state": "provider_complete"}), Some(WENT_ON))
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => Err(self.unfinished(&error).await),
        }
    }

    /// Give the person the turn's answer.
    ///
    /// # Errors
    /// Chat did not keep how the reply ended: it then says on the reply that the answer could
    /// not be finished ([`Self::unfinished`]), and whoever works the turn reads why. No reader
    /// takes such a reply for answered, so the message it answers is not asked twice.
    async fn finished(&self, outcome: &Outcome) -> Result<(), String> {
        self.finished_with(outcome, 0, &TurnUsage::default()).await
    }

    /// [`Self::finished`], recording what the turn's model calls cost and what answered them on
    /// the reply, so a conversation says which provider and model wrote each answer and what it
    /// used — the same reply after its person switched models mid-session.
    async fn finished_with(
        &self,
        outcome: &Outcome,
        cost: u64,
        usage: &TurnUsage,
    ) -> Result<(), String> {
        match &self.ask {
            Ask::Message { .. } => {
                let (state, words) = match outcome {
                    Outcome::Answered(words) => ("provider_complete", words.as_str()),
                    Outcome::Stopped(words) => ("provider_truncated", words.as_str()),
                    Outcome::Failed(words) => ("provider_failed", words.as_str()),
                    Outcome::Cancelled => ("provider_cancelled", ""),
                };
                let grew = self.grown_ends(outcome).await;
                // A reply that grew ended through its stream: only what answered it is kept
                // there now, and a model the server provides itself has nothing to say.
                if grew && usage.answered.is_none() {
                    return Ok(());
                }
                // A reply that grew and then lost a piece takes no more words into its text:
                // the turn's words are kept with its work, so none of them is gone.
                let mut work = if !grew && self.written.lacks_words() {
                    json!({"state": state, "detail": words})
                } else {
                    json!({"state": state})
                };
                if let (Some(answered), Some(fields)) = (&usage.answered, work.as_object_mut()) {
                    fields.insert(
                        "answered".to_owned(),
                        json!({
                            "provider": answered.provider.clone(),
                            "model": answered.model.clone(),
                            "label": answered.label.clone(),
                            "costMicros": cost,
                            "tokens": usage.tokens,
                        }),
                    );
                }
                let say = (!grew).then_some(words);
                match self.work(work, say).await {
                    Ok(()) => Ok(()),
                    Err(error) => Err(self.unfinished(&error).await),
                }
            }
            Ask::Triggered(place) => {
                self.kept().await?;
                let words = match outcome {
                    Outcome::Answered(words) | Outcome::Stopped(words) | Outcome::Failed(words) => {
                        words.trim()
                    }
                    Outcome::Cancelled => "I was stopped.",
                };
                let words = if words.is_empty() {
                    "I finished without saying what I did; look at the history for my changes."
                } else {
                    words
                };
                triggered::write_answer(&self.provider, &self.caller, &self.workspace, place, words)
                    .await
            }
            Ask::Issue { record, issue, .. } => {
                self.kept().await?;
                let words = match outcome {
                    Outcome::Answered(words) | Outcome::Stopped(words) | Outcome::Failed(words) => {
                        words.as_str()
                    }
                    Outcome::Cancelled => "I was stopped.",
                };
                let route = DocumentRoute {
                    app_id: TASKS.to_owned(),
                    cluster_id: self.workspace.clone(),
                    document: record.clone(),
                };
                let mut arguments = Map::new();
                arguments.insert("issue".into(), json!(issue));
                arguments.insert(
                    "text".into(),
                    json!(if words.trim().is_empty() {
                        "I finished without saying what I did; look at the issue's history for my changes."
                    } else {
                        words.trim()
                    }),
                );
                invoke_named(
                    &self.provider,
                    &self.caller,
                    &route,
                    &tasks_model::issue_resource(&tilo_resource::Id::from_raw(issue)),
                    "comment",
                    arguments,
                )
                .await
                .map(drop)
            }
        }
    }

    /// Put the view the model wrote on the reply to the message, through Chat's own
    /// `answer-with-view`, as the agent acting for the person inside this turn: what the model
    /// reads back, and whether it failed. The message is the turn's own ask, and the view's
    /// author is whoever Chat pins from this caller: nothing the model says names either.
    async fn view(&self, input: &Value) -> (String, bool) {
        let Ask::Message { part, message, .. } = &self.ask else {
            return (
                "only an answer in a conversation carries a view".to_owned(),
                true,
            );
        };
        let given = input.as_object().cloned().unwrap_or_default();
        if let Some(other) = given
            .keys()
            .find(|key| !matches!(key.as_str(), "source" | "reads"))
        {
            return (
                format!(
                    "{ANSWER_WITH_VIEW} takes `source` and `reads` and nothing else, not \
                     `{other}`: whose view it is and which message it answers are never arguments"
                ),
                true,
            );
        }
        let (Some(source), Some(reads)) = (
            given.get("source").filter(|source| source.is_string()),
            given.get("reads").filter(|reads| reads.is_array()),
        ) else {
            return (
                format!(
                    "{ANSWER_WITH_VIEW} takes `source`, the view as text, and `reads`, the list \
                     of the apps it reads"
                ),
                true,
            );
        };
        let route = DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: self.workspace.clone(),
            document: part.clone(),
        };
        let mut arguments = Map::new();
        arguments.insert("message".into(), json!(message));
        arguments.insert("source".into(), source.clone());
        arguments.insert("reads".into(), reads.clone());
        let resource = format!("{}/messages/{message}", chat::parts::ChatMessages::KIND);
        match invoke_named(
            &self.provider,
            &self.caller,
            &route,
            &resource,
            "answer-with-view",
            arguments,
        )
        .await
        {
            Ok(_) => (
                "The view is on your reply, shown live to everyone who reads it. Now say in a \
                 sentence or two what it shows."
                    .to_owned(),
                false,
            ),
            Err(why) => (format!("The view was not posted: {why}"), true),
        }
    }

    /// Record `work` on the reply to the message, with `say` as its words when it has none yet.
    ///
    /// # Errors
    /// Chat did not take it, as it says.
    async fn work(&self, work: Value, say: Option<&str>) -> Result<(), String> {
        let Ask::Message { part, message, .. } = &self.ask else {
            return Err("the ask is no message of a conversation".to_owned());
        };
        self.kept().await?;
        let route = DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: self.workspace.clone(),
            document: part.clone(),
        };
        let mut arguments = Map::new();
        arguments.insert("message".into(), json!(message));
        arguments.insert("work".into(), work);
        if let Some(say) = say.map(str::trim).filter(|say| !say.is_empty()) {
            arguments.insert("say".into(), json!(say));
        }
        let resource = format!("{}/messages/{message}", chat::parts::ChatMessages::KIND);
        invoke_named(
            &self.provider,
            &self.caller,
            &route,
            &resource,
            KEEP_WORK,
            arguments,
        )
        .await
        .map(drop)
        .map_err(|error| {
            tracing::warn!(workspace = %self.workspace, %message, %error, "a reply of an agent on the workspace's server did not take its work");
            error
        })
    }

    /// Say on the reply that its answer could not be finished, where Chat still takes a write,
    /// and answer what its person reads there. An end Chat never took leaves the reply unfinished
    /// to every reader, and the message it answers owed a turn again; a failure to say so is the
    /// log's ([`Self::work`]).
    async fn unfinished(&self, why: &str) -> String {
        let said = format!("This answer was not kept as finished: {why}");
        let _ = self
            .work(json!({"state": "provider_failed", "detail": said}), None)
            .await;
        said
    }
}

/// What the agent `caller` is held to in `workspace`, as the workspace's list of its agents
/// keeps it: the bound kept for it there, and nothing at all for one that was retired.
/// Answered for an agent a person's own device handed its session to: one this server works for
/// its keeper on a turn, or one a person's own computer works with under its name (`tilo
/// login`, then `tilo mcp`, whose id is [`tilo_actions::remote::device_agent_id`]).
///
/// A computer's agent the list does not name yet is added as it first arrives or acts: kept by
/// its person and marked as the client it connects as, so the workspace's Agents tab shows it
/// and a bound set there narrows what it does. It is added once, under the name it came with; a
/// name its keeper gives it later stays. The server writes it as the workspace's governor, for
/// the person, as it counts what its own agents spend.
///
/// A list this server cannot read holds the agent to nothing: what it may do is never more than
/// the list says.
pub async fn workspace_agent(
    workspaces: &Workspaces,
    workspace: WorkspaceId,
    caller: &Caller,
) -> Vec<tilo_action_policy::Rule> {
    let (Some(agent), Some(person)) = (
        caller.acting().agent_id(),
        caller.acting().acted_for_person(),
    ) else {
        return Vec::new();
    };
    if !caller.is_session_delegated() {
        return Vec::new();
    }
    let workspace = workspace.to_string();
    if let Some(bound) = workspaces.kept_bound(&workspace, agent).await {
        return bound;
    }
    let (Some(user), Some(name)) = (caller.user_public_key(), caller.agent_name()) else {
        return Vec::new();
    };
    if tilo_actions::remote::device_agent_id(user, name) == agent
        && let Err(error) = list_agent(workspaces, &workspace, (agent, name), person).await
    {
        tracing::warn!(%workspace, %agent, %error, "a computer's agent is not listed in the workspace's agents");
    }
    Vec::new()
}

/// Add the computer's agent `agent` named `name` to `workspace`'s list of its agents, kept by
/// `person`: the list starts with it where the workspace has none yet. Nothing when another
/// arrival added it first.
async fn list_agent(
    workspaces: &Workspaces,
    workspace: &str,
    (agent, name): (&str, &str),
    person: &str,
) -> Result<(), String> {
    let name = tilo_resource::NonBlank::parse(name.trim()).ok_or("the agent has no name")?;
    let (agent, person) = (agent.to_owned(), person.to_owned());
    // A workspace whose people made no agent yet has no list of them: it starts here, named as
    // the list every reader opens.
    workspaces
        .author_agents(workspace, |doc| {
            let agents = tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc));
            // Named as Assistant's change: every receiver decides it against Assistant's surface.
            let mut listing = agents.as_author(&person).in_app(AGENTS_OWNER, "Assistant");
            let listed = listing.state().map_err(|error| error.to_string())?;
            let applied = if listed
                .agents
                .find(&tilo_resource::Id::from_raw(agent.clone()))
                .is_some()
            {
                Ok(())
            } else {
                listing
                    .apply(WorkspaceAgents::CREATE_AGENT.with(
                        tilo_workspace_agent::workspace_agents::CreateAgent {
                            mcp_client: Some(name.as_str().to_owned()),
                            name,
                            id: Some(agent),
                            model: None,
                        },
                    ))
                    .map(drop)
                    .map_err(|error| error.to_string())
            };
            std::mem::swap(doc, listing.doc_mut());
            applied
        })
        .await
}

/// Invoke `action` on `resource` of the record at `route` as `caller`, and wait for it to end.
///
/// # Errors
/// The provider refused or failed the action, or it did not end in time.
pub async fn invoke_named(
    provider: &WorkspaceActionProvider,
    caller: &Caller,
    route: &DocumentRoute,
    resource: &str,
    action: &str,
    arguments: Map<String, Value>,
) -> Result<Value, String> {
    let target = target_named(route, resource, action);
    invoke_at(provider, caller, target, InvokeArgument::Json(arguments)).await
}

/// `action` on `resource` of what is kept at `route`, as what an invocation names.
fn target_named(route: &DocumentRoute, resource: &str, action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: resource_provider(route),
        address: ActionAddress {
            resource: resource.to_owned(),
            action: action.to_owned(),
        },
    }
}

/// What asks for `target` with `argument`: an invocation of its own.
fn invocation_of(target: RemoteActionTarget, argument: InvokeArgument) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target,
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// Invoke `target` with `argument` as `caller`, and wait for it to end.
///
/// # Errors
/// The action was refused or failed, or it did not end in time.
async fn invoke_at(
    provider: &WorkspaceActionProvider,
    caller: &Caller,
    target: RemoteActionTarget,
    argument: InvokeArgument,
) -> Result<Value, String> {
    let request = invocation_of(target, argument);
    let (provider, caller) = (provider.clone(), caller.clone());
    tokio::task::spawn_blocking(move || {
        let invocation = request.invocation;
        let receipt = provider
            .invoke(&caller, request)
            .wait_timeout(WRITE_WAIT)
            .map_err(|_| "the provider did not take the write in time".to_owned())?
            .ok_or("the provider dropped the write")?
            .map_err(|error| error.to_string())?;
        match receipt.status {
            tilo_actions::remote::ReceiptStatus::Completed { output } => {
                Ok(serde_json::to_value(output).unwrap_or(Value::Null))
            }
            tilo_actions::remote::ReceiptStatus::Accepted => {
                let deadline = later(WRITE_WAIT);
                loop {
                    if let Some(state) = provider.observe(&caller, &invocation) {
                        match state.update {
                            InvocationUpdate::Succeeded { output, .. } => {
                                return Ok(serde_json::to_value(output).unwrap_or(Value::Null));
                            }
                            InvocationUpdate::Failed { error, .. } => return Err(error.message),
                            InvocationUpdate::Cancelled { .. } => {
                                return Err("the write was cancelled".into());
                            }
                            _ => {}
                        }
                    }
                    if tilo_clock::now() > deadline {
                        return Err("the write did not finish in time".into());
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            tilo_actions::remote::ReceiptStatus::Failed { error } => Err(error.message),
            tilo_actions::remote::ReceiptStatus::Rejected { error } => Err(error.to_string()),
            tilo_actions::remote::ReceiptStatus::Cancelled { .. } => {
                Err("the write was cancelled".into())
            }
        }
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Work on `turn` with its model through the agent's tools until the model answers, the
/// budget is spent or its person stops it: how it ended, and what it cost. It asks its model for
/// as long as the model calls tools; nothing counts those asks to end the turn.
/// What its person wrote since reaches it at each step ([`going::Going::take`]): `turn` and
/// `reply` are then the newest of it, where the answer goes.
async fn work(
    model: Model,
    scope: &TurnScope,
    turn: &mut Turn,
    budget: Option<u64>,
    reply: &mut Reply,
    going: &going::Going,
    keeping: Option<&learned::Keeping>,
    web: web_search::Web,
) -> (Outcome, u64, TurnUsage) {
    let Some(turns) = tokio::task::spawn_blocking(move || model())
        .await
        .ok()
        .flatten()
    else {
        return (
            Outcome::Failed(
                "This workspace's server has no model its agents can work with, so I cannot work on this."
                    .to_owned(),
            ),
            0,
            TurnUsage::default(),
        );
    };
    let server = match McpServer::for_turn(reply.provider.clone(), reply.caller.clone())
        .map(|server| server.with_observation(steps::observed()))
    {
        Ok(server) => server,
        Err(error) => return (Outcome::Failed(error.to_string()), 0, TurnUsage::default()),
    };
    // What the turn keeps about its person, in a conversation the view it may answer with, and
    // the public web it may look up, are its own tools beside the agent tools.
    let mut own_tools = learned::with_tools(Vec::new(), keeping);
    own_tools.extend([view_tool(turn), shares::tool(turn)].into_iter().flatten());
    own_tools.extend(web_search::definitions());
    let asked = tilo_agent_turn::Asked {
        history: instructions::history(&turn.history),
        own_tools,
        sees: turns.sees(),
        budget,
        most_rounds: None,
        output_tokens: ANSWER_TOKENS,
        context_chars: turns.context_chars().unwrap_or(ASKED_CONTEXT_CHARS),
        result_bytes: MOST_RESULT_BYTES,
        round_time: TURN_TIME,
        key: format!("{}:{}", turn.agent.id.0, turn.cause),
        ..tilo_agent_turn::Asked::new(instructions::for_turn(turn), turn.request.clone())
    };
    let agent = turn.agent.id.0.clone();
    let usage = Arc::new(Mutex::new(TurnUsage::default()));
    let model = Arc::new(Mutex::new(Asking {
        usage: Arc::clone(&usage),
        turns: asking::again(turns, going.stop_flag()),
    }));
    let answering = Answering {
        scope,
        going,
        keeping,
        reads: Mutex::new(steps::reads(&server).await),
        noted: Mutex::default(),
        at: tokio::sync::Mutex::new((turn, reply)),
        server: &server,
        web: &web,
    };
    // Every line the turn writes names its workspace and its agent: a server runs the turns of
    // many workspaces at once, and its log says whose step each line is.
    let span = tracing::info_span!("turn", workspace = %scope.workspace, %agent);
    let (outcome, cost) = tilo_agent_turn::work(&server, model, &asked, &answering)
        .instrument(span)
        .await;
    if let Outcome::Answered(words) = &outcome {
        tracing::info!(%agent, cost, said = words.len(), "an agent on the workspace's server answered");
    }
    let usage = usage.lock_recover().clone();
    (outcome, cost, usage)
}

/// A turn's model as the turn asks it: each call asked again where it fails for a moment
/// ([`asking::again`]), and what it writes handed on as it comes. What a call answered with
/// and used is kept, for the reply that names it.
struct Asking {
    usage: Arc<Mutex<TurnUsage>>,
    turns: Box<dyn PlanTurns>,
}

impl tilo_agent_turn::Model for Asking {
    fn turn(
        &mut self,
        request: &ModelRequest,
        key: &str,
        deadline: Instant,
    ) -> Result<ModelResponse, String> {
        self.turn_streaming(request, key, deadline, &mut |_| {})
    }

    fn turn_streaming(
        &mut self,
        request: &ModelRequest,
        key: &str,
        deadline: Instant,
        heard: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<ModelResponse, String> {
        let answer = self
            .turns
            .turn_streaming(request, key, deadline, false, heard)
            .map_err(|error| format!("My model did not answer: {}", error.message))?;
        let mut usage = self.usage.lock_recover();
        if let Some(answered) = self.turns.answered_by() {
            usage.answered = Some(answered);
        }
        usage.tokens = usage
            .tokens
            .saturating_add(answer.usage.input_tokens)
            .saturating_add(answer.usage.output_tokens);
        drop(usage);
        Ok(answer)
    }
}

/// How a turn on the workspace's server tells its person what it does: on the reply to their
/// ask, which moves to the newest of what they wrote as the turn goes on with it.
struct Answering<'a> {
    scope: &'a TurnScope,
    going: &'a going::Going,
    /// What a turn that knows and keeps facts keeps of what it learns ([`learned`]).
    keeping: Option<&'a learned::Keeping>,
    /// Which of the turn's tools only read, by its server's own word of each.
    reads: Mutex<HashSet<String>>,
    /// Of the answer whose calls are being carried out: the tool of the call before, and the
    /// read whose note has not gone to the reply yet.
    noted: Mutex<(Option<String>, Option<String>)>,
    /// The ask the turn answers now, and where its answer is written.
    at: tokio::sync::Mutex<(&'a mut Turn, &'a mut Reply)>,
    /// The turn's agent tools, which a document it answers with is read through.
    server: &'a McpServer<WorkspaceActionProvider>,
    /// The public web the turn may look up in ([`web_search`]).
    web: &'a web_search::Web,
}

impl tilo_agent_turn::Doing<McpServer<WorkspaceActionProvider>> for Answering<'_> {
    fn stopped(&self) -> bool {
        self.going.stopped()
    }

    /// The note of a step holds no tool back ([`Reply::notes`]). Calls of one tool that follow
    /// each other are noted once. A tool that only reads is noted when the calls of its answer
    /// are done, the newest read alone: the service takes every tool's requests before a note's
    /// write, and the note is written while the model reads the results. Any other tool is
    /// noted before it begins: its person reads what is being done while it is done, and
    /// whoever finds the turn unfinished reads that it may have changed something. What the
    /// step costs is said in the log when it ends ([`steps`]).
    async fn carry_out<W>(
        &self,
        call: &ToolCall,
        step: tilo_agent_turn::Step,
        work: W,
    ) -> tilo_agent_turn::Answer
    where
        W: Future<Output = tilo_agent_turn::Answer> + Send,
    {
        if step.nth == 0 {
            *self.noted.lock_recover() = (None, None);
        }
        let at = self.at.lock().await;
        let noting = if !matches!(at.1.ask, Ask::Message { .. }) {
            steps::Note::Nowhere
        } else if self.noted.lock_recover().0.as_deref() == Some(call.name.as_str()) {
            steps::Note::Same
        } else if self.reads.lock_recover().contains(&call.name) {
            steps::Note::After
        } else {
            steps::Note::Before
        };
        let mut timed = steps::Step::begin(&call.name, (step.nth, step.of), noting);
        if noting == steps::Note::Before {
            // Waited for, as it always was: handed on, it would stand in front of the tool's
            // own action on the workspace's thread, inside what the tool waits for that action.
            self.noted.lock_recover().1 = None;
            at.1.doing(&narration::said(call)).await;
            timed.noted();
        }
        // The view tool writes on the reply itself.
        drop(at);
        let answer = Box::pin(timed.carried_out(work)).await;
        {
            let mut noted = self.noted.lock_recover();
            if noting == steps::Note::After {
                noted.1 = Some(narration::said(call));
            }
            noted.0 = Some(call.name.clone());
        }
        timed.say(answer.text.len(), answer.is_error);
        // A line of the log names the call's tool and each argument by its name and the bytes
        // of its value; of a refusal it keeps the sentence without anything the call wrote, and
        // of an answer its size ([`tilo_agent_turn::logged`]). No value of an argument and
        // nothing of an answer stands in the log, at any level.
        let tool = tilo_agent_turn::logged::offered_name(&call.name);
        let arguments = tilo_agent_turn::logged::arguments(&call.input);
        if answer.is_error {
            let said = tilo_agent_turn::logged::refusal(&answer.text, &call.input);
            tracing::info!(%tool, %arguments, %said, "a tool of an agent on the workspace's server was refused or failed");
        } else {
            tracing::info!(%tool, "an agent on the workspace's server used a tool");
        }
        tracing::debug!(%tool, %arguments, answered_bytes = answer.text.len(), "what a tool of an agent on the workspace's server answered");
        if step.last() {
            let unsaid = self.noted.lock_recover().1.take();
            if let Some(read) = unsaid {
                self.at.lock().await.1.notes(&read);
            }
        }
        answer
    }

    /// What the turn keeps about its person is carried out here, the view or document it answers
    /// with, and the public web it looks up; every other tool is the agent tools'.
    async fn own(&self, call: &ToolCall) -> Option<tilo_agent_turn::Answer> {
        let (text, is_error) = if call.name == ANSWER_WITH_VIEW {
            let at = self.at.lock().await;
            Box::pin(at.1.view(&call.input)).await
        } else if call.name == shares::ANSWER_WITH_DOCUMENT {
            let at = self.at.lock().await;
            Box::pin(at.1.document(self.server, &call.input)).await
        } else if let Some(web) = self.web.tool(&call.name, &call.input).await {
            web
        } else {
            learned::call(self.keeping, &call.name, &call.input).await?
        };
        Some(tilo_agent_turn::Answer {
            text,
            pictures: Vec::new(),
            is_error,
        })
    }

    /// What the model writes and thinks goes into the reply as it comes ([`written`]).
    async fn hear(&self, mut hearing: tilo_agent_turn::Hearing) {
        let writes = written::Writes::default();
        let pumped = async {
            while let Some(pieces) = hearing.next().await {
                for piece in pieces {
                    match piece {
                        StreamEvent::ReasoningDelta { text } => writes.thought(&text),
                        StreamEvent::TextDelta { text } => writes.push(&text),
                        // The call broke off and may be asked again: what comes then stands
                        // below.
                        StreamEvent::Failed { .. } => writes.broke_off(),
                        _ => {}
                    }
                }
            }
        };
        let at = self.at.lock().await;
        written::follow(&*at.1, pumped, &writes).await;
        drop(at);
    }

    /// What of the answer's words its provider said only at the end goes into the reply too.
    async fn heard(&self, response: &ModelResponse) {
        let at = self.at.lock().await;
        if let Some(rest) = written::Takes::lacks(&*at.1, &tilo_agent_turn::words_of(response)) {
            written::Takes::take(&*at.1, &rest).await;
        }
        drop(at);
    }

    fn left_off(&self, why: String, messages: &[Message]) -> String {
        asking::left_off(&why, messages)
    }

    /// Take into the turn what its person wrote since its last step: the replies the turn leaves
    /// say the answer follows below, and the turn goes on as the newest of it, inside that ask.
    async fn went_on(
        &self,
    ) -> Result<Option<tilo_agent_turn::WentOn<McpServer<WorkspaceActionProvider>>>, String> {
        let came = self.going.take();
        let Some((newest, passed)) = came.split_last() else {
            return Ok(None);
        };
        let mut at = self.at.lock().await;
        let (turn, reply) = &mut *at;
        reply.went_on().await?;
        for between in passed {
            self.scope.reply(between).went_on().await?;
        }
        **turn = newest.clone();
        **reply = self.scope.reply(turn);
        reply.taken().await;
        let server = McpServer::for_turn(reply.provider.clone(), reply.caller.clone())
            .map(|server| server.with_observation(steps::observed()))
            .map_err(|error| error.to_string());
        drop(at);
        let server = server?;
        *self.reads.lock_recover() = steps::reads(&server).await;
        let told = came
            .iter()
            .map(|next| next.request.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        Ok(Some(tilo_agent_turn::WentOn {
            told,
            tools: Some(server),
        }))
    }

    fn waiting(&self) -> bool {
        self.going.waiting()
    }
}

/// `wait` from now, or now when that is past what a clock reads.
fn later(wait: Duration) -> Instant {
    let now = tilo_clock::now();
    now.checked_add(wait).unwrap_or(now)
}

/// The tool a turn in a conversation answers with a view through, as the model is told it: when
/// a view is the right answer and how one is written ([`chat::message_view::GUIDE`]). None for
/// an issue's turn, whose answer is a comment.
fn view_tool(turn: &Turn) -> Option<ToolDefinition> {
    matches!(turn.ask, Ask::Message { .. }).then(|| ToolDefinition {
        name: ANSWER_WITH_VIEW.to_owned(),
        description: chat::message_view::GUIDE.to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "source": {"type": "string"},
                "reads": {"type": "array", "items": {"type": "string"}},
            },
            "required": ["source", "reads"],
            "additionalProperties": false,
        }),
    })
}

/// The record `doc` holds, read as this build's `R` once it is brought up to
/// `R`'s storage version by the app's own migration
/// ([`tilo_resource::evolution::migrate`]). A workspace an older server or older
/// devices wrote is read, never refused: a newer server starts on an older
/// server's state. The migration deletes nothing and rewrites no id, and it
/// is the same change every device migrating the record makes.
///
/// # Errors
/// The migration failed, or the migrated record does not read as `R`.
pub fn opened<R>(doc: &mut automerge::AutoCommit) -> Result<R::State, String>
where
    R: tilo_resource::Resource,
{
    tilo_resource::evolution::migrate::<R>(doc)
        .map_err(|error| format!("bring the record up to version {}: {error}", R::VERSION))?;
    R::read(doc).map_err(|error| error.to_string())
}

/// The record of `app` a workspace keeps and seeds: the primary of an app that
/// keeps one per workspace (chat), else the workspace's default record, which every
/// member's first window of the app opens.
#[must_use]
pub fn workspace_resource(catalogue: &tilo_agent_session::AppCatalogue, app: &str) -> String {
    match catalogue.get(app) {
        Some(definition) if !definition.keeps_workspace_resource() => {
            collab_types::document::DocumentId::workspace_default()
                .as_str()
                .to_owned()
        }
        _ => String::new(),
    }
}

/// Today, `YYYY-MM-DD`, as an agent's keeper counts the days its spending is charged to.
pub fn today() -> String {
    tilo_clock::local::today().to_string()
}

/// Begin an agent session on the task of Tasks `turn` was triggered on: Tasks is told it began,
/// its place is made, and `turn` is told what the session needs. The task's work as it stood
/// before; none for a turn that is no task's, or whose task this turn cannot read.
///
/// # Errors
/// Tasks refused the session (its budget is spent, or its sessions were lost too often): its
/// plain line, which the task's work is stopped with.
async fn task_session(
    turn: &mut Turn,
    reply: &Reply,
) -> Result<Option<(issue_work::TaskWork, issue_work::Before)>, String> {
    let Ask::Triggered(place) = &turn.ask else {
        return Ok(None);
    };
    if place.app != tl_tasks::APP {
        return Ok(None);
    }
    let Some(reads) = reply.provider.resource_reads() else {
        return Err("this device reads no task for an agent".to_owned());
    };
    let task = issue_work::TaskWork {
        service: reply.provider.clone(),
        caller: reply.caller.clone(),
        reads,
        workspace: reply.workspace.clone(),
        task: place.item.clone(),
    };
    Ok(task
        .begin(&mut turn.request)
        .await?
        .map(|before| (task, before)))
}

/// `micros` millionths of the currency, as a person reads an amount.
fn money(micros: u64) -> String {
    format!(
        "${}.{:02}",
        micros / 1_000_000,
        (micros % 1_000_000) / 10_000
    )
}
