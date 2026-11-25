//! The one engine that runs a workspace's agents: on the workspace's host, around the clock, for
//! the workspaces it governs, and on a person's own device for the workspaces it holds
//! ([`run_on_device`]).
//!
//! Who they are is the workspace's own record of its agents (Assistant's
//! `tilo.workspace.agent`): every active agent no MCP client connects as works here, whoever
//! made it. What they are asked is what the workspace's people
//! write: a message of Chat in a conversation the agent takes part in or that mentions it, and a
//! task of Tasks given to the agent, which its triggers start a turn on ([`triggered`],
//! [`task_work`]). A message asks too when something else wrote it for a person (a reminder
//! that is due, an event that fired) into their conversation with the agent. Chat's records hold
//! every ask and every answer, so the host keeps nothing of its own.
//!
//! A turn starts from the agent's triggers ([`triggered`]): a change one of them fires on
//! invokes the agent's answer, and the engine takes the turn from that change alone
//! ([`ServerAgents::run`]). Nothing scans the workspace for what is owed.
//!
//! An agent has one turn at a time in a conversation ([`going`]). What its person writes while
//! it works reaches that turn at its next step: it is told it and goes on under it, where its
//! answer then stands. The reply is a message of Chat that names the turn's `#answer`
//! invocation (its `turn`), streamed into as the model writes ([`Reply`]). The person stops the
//! turn with Chat's Stop, which cancels that invocation: the turn ends at its next step, a model
//! call under way included, and its reply says it was stopped.
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

use std::collections::{BTreeMap, BTreeSet};
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
    Message, ModelError, ModelRequest, ModelResponse, StreamEvent, ToolCall,
};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::WorkspaceId;
use tilo_workspace_agent::{Agent, WorkspaceAgents};
use tracing::Instrument as _;

use crate::workspaces::Workspaces;
use crate::workspaces::model_answers::{ModelAnswerer, ModelRefusal, Spender};
use agent_keys::AgentKeys as _;
use turn_provider::TurnProvider;

pub mod agent_keys;
pub mod assistant;
pub mod events;
pub mod initiative;
pub mod task_work;
pub mod telling;
pub mod ticketed_keys;
pub mod triggered;

mod across;
mod asking;
#[cfg(test)]
mod device_turn_tests;
mod engine_actions;
mod going;
mod instructions;
mod kept;
mod learned;
#[cfg(feature = "agents")]
mod moderation_history;
#[cfg(test)]
mod moderation_turn_tests;
mod shares;
mod steps;
mod sub_agents;
mod team;
mod turn_provider;
mod turn_tools;
mod vault_uses;
pub mod web_search;

// A turn's open of Browser, where this server places the window.
mod turn_browser;
// What a turn does at the page in that window.
mod turn_pages;
// The Browser windows a host places, as a turn reaches them.
pub mod windows;

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

/// The app that keeps the conversations.
const CHAT: &str = triggered::CHAT;
/// The app that keeps the workspace's list of its agents.
const AGENTS_OWNER: &str = tilo_workspace_agent::OWNER;
/// The messages before the one that asked a turn is told, at most: what was said around it.
const RECALLED: usize = 40;
/// How long a reply's words may stand unwritten while its model writes them: what streamed in
/// meanwhile goes into the reply together.
const STREAM_EVERY: Duration = Duration::from_millis(400);
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

/// The provider serving one workspace's actions as an [`ActionProvider`]: what the agent's
/// tools run on, and what its replies are written through.
#[derive(Clone)]
pub struct WorkspaceActionProvider {
    actions: EngineActions,
    workspace: WorkspaceId,
    /// What this service answers itself for the one turn it serves, beside the workspace's
    /// resources ([`TurnProvider`]).
    own: Vec<Arc<dyn TurnProvider>>,
    /// How the calls those took ended; `None` while it answers none of them.
    ended: Option<turn_provider::Ended>,
    /// What the person asked of the turn this serves, in their words: the purpose History
    /// keeps with each change the turn makes, so its agent session reads as what it was asked.
    purpose: Option<String>,
}

impl WorkspaceActionProvider {
    #[must_use]
    pub fn new(actions: impl Into<EngineActions>, workspace: WorkspaceId) -> Self {
        Self {
            actions: actions.into(),
            workspace,
            own: Vec::new(),
            ended: None,
            purpose: None,
        }
    }

    /// What `caller` is offered of the workspace's records and the providers the device's
    /// dispatch answers, as the workspace's policy decides it for a turn this server carries
    /// out for them.
    async fn page(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Result<CataloguePage, ActionCallError> {
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
                    page.providers.extend(offered);
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
        let answer = self.actions.serve(caller, message).unwrap_or_else(|| {
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

/// Run the agents of the workspaces a person's own device holds, `workspaces`, in the device's
/// one daemon: the one engine a host runs, with its turns' calls decided by the device's dispatch
/// (`actions`) as the person's own calls are. It takes every turn its workspaces' records ask of
/// their agents that is this device's to take ([`ServerAgents::run`],
/// [`crate::work::takes_turn`]). Where the device serves tickets (`tickets`), it asks with one to
/// run the agents of other people it may take turns of, and runs each once its person redeemed
/// the ticket. What the assistants' look-round (`initiative`) proposes is filed through the same
/// calls, each signed by its agent's key.
pub async fn run_on_device(
    workspaces: Workspaces,
    actions: Arc<dyn ActionProvider + Send + Sync>,
    tickets: Option<Arc<dyn ticketed_keys::AgentKeyTickets>>,
    initiative: Option<initiative::Initiative>,
) {
    // The person's own device calls the model their workspace linked where it is, one on their
    // own network or on this machine included: a local model answers with no network at all.
    workspaces.call_private_model_addresses(true);
    // The model this computer's Agent models pane chose is the workspaces' it keeps.
    workspaces.on_a_computer();
    workspaces.follow_this_computer();
    telling::start(&workspaces);
    // The records of the tilolang apps built in (tl Chat, Tasks) answer here where no window of
    // their app is open: what an agent's reply and its tools write through.
    match workspaces.tilolang() {
        Some(tilolang) => tilolang.records.serve(&workspaces),
        None => tracing::warn!(
            "this daemon runs no tilolang runtime: no built-in app's record is served, and an \
             agent's reply reaches none"
        ),
    }
    // The vault's own actions (the code of the moment, keeping a seed and forgetting it) are
    // what an app that shows one-time codes calls: served here, where the vault is.
    crate::vault_actions::register(&workspaces);
    // Each agent signs its calls with a key of its own: made here from its person's root, or,
    // on a device without that root (a server that keeps the workspace), the key this device
    // asked to run the agent with once its person approved it.
    let roots = match agent_keys::this_device() {
        Ok(roots) => Some(roots),
        Err(error) => {
            tracing::info!(
                "this device makes no agent key from a root ({error}): it runs the agents whose \
                 people let it"
            );
            None
        }
    };
    let keys = Arc::new(ticketed_keys::DeviceAgentKeys::of(&workspaces, roots));
    // Every key an agent is given here from a root is kept among its person's keys in their own
    // workspace.
    keys.keep_in(&workspaces);
    if let Some(initiative) = initiative {
        initiative.act_through(EngineActions::new(
            Arc::clone(&actions),
            Arc::clone(&keys) as Arc<dyn agent_keys::AgentKeys>,
        ));
    }
    tracing::info!(
        "this device runs its workspaces' agents, each on the model its workspace linked"
    );
    // The agents speak the language their person reads this computer in.
    let agents = ServerAgents::on_device(workspaces, actions, keys.clone())
        .speaking(settings_store::appearance::preferred_language);
    match tickets {
        Some(tickets) => agents.asking_with(keys, tickets),
        None => agents,
    }
    .run()
    .await;
}

/// How often a device looks again at where its workspaces' agents run, with nothing written
/// meanwhile: a certificate close to running out is asked for again.
const RUNS_ON_AGAIN: Duration = Duration::from_secs(60 * 60);

/// The language a host's agents speak, asked anew for each turn ([`ServerAgents::speaking`]).
pub type Language = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// What carries one turn's model calls, made fresh for each turn on its own thread: a test's
/// script, standing where a workspace's model would ([`ServerAgents::scripted`]).
pub type Model = Arc<dyn Fn() -> Option<Box<dyn PlanTurns>> + Send + Sync>;

/// Something a person asked an agent, and where its answer goes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ask {
    /// A change one of the agent's triggers fired on ([`triggered`]): the answer is written
    /// where the change was made, a reply in its conversation, an answer to the mail
    /// conversation it arrived in, or a comment on its task.
    Triggered(triggered::Where),
}

impl Ask {
    /// The message of Chat it is, as the conversation it is in and its id there; none for a
    /// task.
    fn message(&self) -> Option<&triggered::Where> {
        match self {
            Self::Triggered(place) if place.app == CHAT => Some(place),
            Self::Triggered(_) => None,
        }
    }
}

/// A message said before the one that asked a turn, as the turn is told it: who said it, and
/// what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recalled {
    /// Whether the turn's own agent said it.
    pub by_agent: bool,
    /// Who said it, by the name the conversation shows.
    pub by: String,
    pub text: String,
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
    /// What was said in the conversation before this request: its summary first, where it
    /// has one, then the messages before it.
    pub history: Vec<Recalled>,
    /// What they asked, as a person reads it: what the agent's session is labelled by.
    pub asked: String,
    /// The `#answer` invocation that started the turn, by its id: the reply names it as its
    /// `turn`, and Chat's Stop cancels it. None for a turn no invocation started.
    pub invocation: Option<String>,
    /// Whether the person wrote it themselves, rather than a cause asking for them: their own
    /// words reach a turn of theirs that is going.
    pub typed: bool,
    /// The person who saved the shortcut whose trigger fired, where that is not the person the
    /// change answers ([`Turn::person`]): one more link in the line of who asked, bounding what
    /// the turn may do. None for a turn no saved shortcut started, or where one person did both.
    pub saved_by: Option<String>,
    /// Whether it was asked where only the person and the agent read: their own direct
    /// conversation. What the agent learns of them is kept and read only in such a turn.
    pub private: bool,
    /// Where a sub-agent's turn stands in its team ([`Line`]): none for an agent a person made.
    pub line: Option<Line>,
    /// The task of Tasks the turn works on, as the change that triggered it left the task: an
    /// agent session on it is begun, placed and counted around the turn ([`task_work`]). None
    /// for a turn on anything else.
    pub task: Option<task_work::Task>,
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

/// What each agent acts under: the key the person's device derives for it from their root
/// ([`agent_keys`]).
type Roster = Arc<dyn agent_keys::AgentKeys>;

/// `agent` of `workspace` as its own key, working for `person`: what everything it does there
/// goes as, each invocation signed by that key.
///
/// # Errors
/// The agent belongs to no person, or has no key of its own on this device.
async fn agent_caller(
    roster: &Roster,
    workspace: WorkspaceId,
    agent: &Agent,
    person: [u8; 32],
    owner: [u8; 32],
) -> Result<Caller, String> {
    let root = tilo_action_policy::PersonId::parse(&agent.acts_for)
        .ok()
        .and_then(|its| its.root())
        .ok_or("the agent belongs to no person")?;
    let key = roster.grant_for(workspace, agent)?;
    // A device that is not the agent's person's hands its session to the agent: the change is
    // the agent's, acting with its person's preset, and the device carried it out.
    let handed = root != owner;
    let caller = if handed {
        Caller::handed_agent(agent.id.0.clone(), root, key, owner)
    } else {
        Caller::delegated_agent(agent.id.0.clone(), root, key)
    };
    // The agent carries the bound its record keeps, which the device's broker holds it to.
    Ok(caller
        .in_workspace(workspace.to_string())
        .named(&agent.name)
        .on_roster(agent.created_at, person))
}

/// Which agents an engine acts as: those whose key the device makes from a root it holds. The agent's own
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
    place: triggered::Where,
    event: tilo_automation::dispatch::DocumentEvent,
    saver: String,
    /// The `#answer` invocation, and the stop its cancel sets.
    invocation: (InvocationId, Arc<AtomicBool>),
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
            // A paused agent's key takes no turn of it until it is resumed.
            if !agent.is_active() {
                return false;
            }
            agent_caller(
                &self.roster,
                id,
                agent,
                [0; 32],
                workspaces.device_cert().user_public_key,
            )
            .await
            .is_ok()
        })
    }

    fn answering(&self) -> Vec<(String, String)> {
        self.answering.lock_recover().clone()
    }

    fn answer(
        &self,
        workspace: String,
        agent: String,
        place: triggered::Where,
        event: tilo_automation::dispatch::DocumentEvent,
        saver: String,
        invocation: (InvocationId, Arc<AtomicBool>),
    ) -> Pending<Result<crate::work::Started, String>> {
        let (started, answer) = pending();
        let asked = Answer {
            workspace,
            agent,
            place,
            event,
            saver,
            invocation,
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
    /// Where this device asks with a ticket to run other people's agents, with the keys it
    /// asks for ([`Self::asking_with`]): `None` where it serves no tickets.
    asking: Option<(
        Arc<ticketed_keys::DeviceAgentKeys>,
        Arc<dyn ticketed_keys::AgentKeyTickets>,
    )>,
}

impl ServerAgents {
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
            EngineActions::new(actions, Arc::clone(&keys)),
            None,
            keys,
        )
    }

    /// Ask `request` of the device's own dispatch as `caller`, signed with its agent's key:
    /// what a turn asks of what its workspace keeps for the person who asked it, decided there
    /// as every other call of the device's ([`crate::workspaces::hosted_use::answers`]).
    ///
    /// # Errors
    /// The call could not be signed as the agent, or the dispatch refused it.
    async fn ask_device(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let mut signed = request.clone();
        self.actions
            .sign_for(workspace, caller, &mut signed)
            .map_err(|reason| ActionCallError::Refused { reason })?;
        let receipt = self
            .workspaces
            .device_actions()
            .invoke(caller, signed)
            .await
            .unwrap_or(Err(ActionCallError::Unanswered))?;
        let invocation = receipt.invocation;
        match receipt.status {
            ReceiptStatus::Completed { output } => {
                Ok(InvocationUpdate::Succeeded { invocation, output })
            }
            ReceiptStatus::Rejected { error } => Err(error),
            ReceiptStatus::Failed { error } => Ok(InvocationUpdate::Failed { invocation, error }),
            ReceiptStatus::Cancelled { message } => Ok(InvocationUpdate::Cancelled {
                invocation,
                message,
            }),
            ReceiptStatus::Accepted => Err(ActionCallError::Unanswered),
        }
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
        Self {
            workspaces,
            actions,
            roster,
            model,
            language: None,
            going: Arc::default(),
            answering,
            answers: Arc::new(Mutex::new(Some(asked))),
            browsers: None,
            placed: None,
            #[cfg(any(test, feature = "fixture-servers"))]
            web_routes: None,
            asking: None,
        }
    }

    /// Ask with a ticket `tickets` serves to run each agent of another person's this device may
    /// take turns of, for a key of `keys` (the keys these agents sign with), and run it once its
    /// person redeemed the ticket.
    #[must_use]
    pub fn asking_with(
        mut self,
        keys: Arc<ticketed_keys::DeviceAgentKeys>,
        tickets: Arc<dyn ticketed_keys::AgentKeyTickets>,
    ) -> Self {
        self.asking = Some((keys, tickets));
        self
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
            return agent_caller(
                &self.roster,
                workspace,
                agent,
                person,
                self.workspaces.device_cert().user_public_key,
            )
            .await;
        }
        let record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let roster = self
            .read::<WorkspaceAgents>(&workspace.to_string(), AGENTS_OWNER, &record)
            .await?
            .ok_or("the workspace keeps no list of its agents")?;
        // A line whose head is gone ends at the sub-agent itself, which holds no approval:
        // nothing is done in its name then.
        let approved = roster.root_of(agent);
        Ok(agent_caller(
            &self.roster,
            workspace,
            approved,
            person,
            self.workspaces.device_cert().user_public_key,
        )
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

    /// These agents, whose turns reach the Browser windows `browsers` places: a turn's open of
    /// Browser has its window placed in the turn's workspace, and a turn has a field of the
    /// page in its workspace's window filled from its person's vault by reference. With `None`
    /// such an open is refused in words that say this server has no machine for a window.
    #[must_use]
    pub fn placing(mut self, browsers: Option<Arc<dyn windows::Windows>>) -> Self {
        self.browsers = browsers;
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
        // Where agents run is looked at again now and then, with nothing written meanwhile: a
        // certificate this device's person issued is renewed before it runs out.
        let mut again = tokio::time::interval(RUNS_ON_AGAIN);
        again.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = again.tick() => self.read_answering(&mut known).await,
                answer = asked.recv() => {
                    let Some(Answer { workspace, agent, place, event, saver, invocation, started }) = answer else {
                        return;
                    };
                    let agents = self.clone();
                    tokio::spawn(async move {
                        let taken = Box::pin(agents.triggered(&workspace, &agent, place, event, saver, invocation)).await;
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

    /// Ask, where this device offers its person's workspaces compute (`offers`) and serves
    /// tickets, to run each agent that works in `workspace` whose person's root is not here, its
    /// own person's included: a ticket shows on the agent until its person redeems it, and one
    /// close to running out is asked for again.
    async fn ask_to_run(&self, workspace: &str, agents: &WorkspaceAgents, offers: bool) {
        let (Some((keys, tickets)), true, Some(id)) =
            (&self.asking, offers, WorkspaceId::parse(workspace))
        else {
            return;
        };
        for agent in agents
            .agents
            .iter()
            .filter(|agent| agent.is_active() && agent.sub.is_none() && agent.mcp_client.is_none())
        {
            if !keys.from_root(id, agent) {
                keys.ticketed()
                    .ask_unless_approved(tickets, id, agent)
                    .await;
            }
        }
    }

    /// Read again which agents the workspaces here keep: each one's answer is what a trigger
    /// reaches ([`EngineKeys::answering`]). `known` holds the agents each workspace kept at the
    /// last read, `None` before the first: an agent a person made since comes with its triggers
    /// ([`triggered::triggers`]) unless the action that made it saved them, and one retired or
    /// removed since takes its triggers with it.
    async fn read_answering(&self, known: &mut Option<BTreeMap<String, BTreeSet<String>>>) {
        let record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let me = self.workspaces.device_cert().user_public_key;
        let mut answering = Vec::new();
        let mut now = BTreeMap::new();
        let offers = crate::work::offers_compute(&self.workspaces).await;
        for workspace in self.workspaces.described().await {
            let id = workspace.id().to_string();
            let Ok(Some(agents)) = self
                .read::<WorkspaceAgents>(&id, AGENTS_OWNER, &record)
                .await
            else {
                continue;
            };
            // A sub-agent answers too: its starter gives it its job and its notes as messages
            // that mention it, in the thread of the starter's reply.
            let answers = |agent: &&Agent| {
                agent.state == tilo_workspace_agent::AgentState::Active
                    && agent.mcp_client.is_none()
            };
            // An agent that was paused keeps its triggers while it waits: the turn it would
            // start is refused until it is resumed, and no shortcut need be saved again.
            let kept = |agent: &&Agent| {
                !agent.is_retired() && agent.mcp_client.is_none() && agent.sub.is_none()
            };
            let tracked: BTreeSet<String> = agents
                .agents
                .iter()
                .filter(kept)
                .map(|agent| agent.id.0.clone())
                .collect();
            if let Some(before) = known.as_ref().and_then(|known| known.get(&id)) {
                let saved: BTreeSet<String> = workspace
                    .shortcuts
                    .iter()
                    .map(|saved| saved.id().to_owned())
                    .collect();
                // The typed create action saves a made agent's triggers itself; an agent made
                // by an app's own change or a sub-agent is saved here, once, as this person's.
                let mine = |agent: &Agent| {
                    tilo_action_policy::PersonId::parse(&agent.acts_for)
                        .is_ok_and(|person| person.root() == Some(me))
                };
                for agent in agents.agents.iter().filter(kept) {
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
                for gone in before.difference(&tracked) {
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
            self.ask_to_run(&id, &agents, offers).await;
            answering.extend(
                agents
                    .agents
                    .iter()
                    .filter(answers)
                    .map(|agent| (agent.id.0.clone(), agent.name.clone())),
            );
            now.insert(id, tracked);
        }
        *known = Some(now);
        answering.sort();
        answering.dedup_by(|a, b| a.0 == b.0);
        *self.answering.lock_recover() = answering;
    }

    /// Start `agent`'s turn in `workspace` on `event`, the change a trigger `saver` keeps fired
    /// on ([`triggered`]), as the `#answer` invocation `invocation`, whose cancel sets the stop
    /// beside it: whether this device took it, once the turn ended. The turn is named by the
    /// agent and the item the change made, so every device that saw it names one turn and one
    /// device takes it ([`crate::work::takes_turn`]). The person it is for is whoever made the
    /// change; for a change made for nobody who is a member (a message a service carried in from
    /// outside), the person whose trigger fired, so the turn may do no more than they may.
    ///
    /// Where a turn of the agent goes in the same place already, what its person wrote reaches
    /// that turn, which goes on with it and answers it: this answer ends when that turn does,
    /// and its cancel stops it too. Anything else waits until that turn ended, and begins then.
    ///
    /// # Errors
    /// The change is nothing an agent answers, or the agent is none of the workspace's.
    pub async fn triggered(
        &self,
        workspace: &str,
        agent: &str,
        place: triggered::Where,
        event: tilo_automation::dispatch::DocumentEvent,
        saver: String,
        (invocation, stop): (InvocationId, Arc<AtomicBool>),
    ) -> Result<crate::work::Started, String> {
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
        // A paused agent answers nothing: the turn its trigger would start is refused here, so
        // resuming it makes the same trigger start one again.
        if !agent.is_active() {
            return Err(format!("the agent `{}` does not work now", agent.id.0));
        }
        // A task is worked on over many agent sessions: the turn is told the task, and its
        // session is begun, placed and counted around it.
        let task = task_work::Task::of(&self.workspaces, workspace, &event).await;
        let request = match &task {
            Some(task) => task_work::asked(&event, task),
            None => triggered::said(&event),
        };
        let title = match &task {
            Some(task) => task.title.clone(),
            None => request.chars().take(120).collect(),
        };
        let line = sub_agents::line_of(&roster, &agent, &event);
        // Who asked: whoever made the change, as every one of the agent's own triggers answers
        // it. Where the shortcut that fired was saved by someone else, that person asked too,
        // and both bound what the turn does.
        let asker = triggered::asker(&event);
        let person = asker.clone().unwrap_or_else(|| saver.clone());
        let saved_by = asker.filter(|asker| asker != &saver).map(|_| saver.clone());
        let agent_id = agent.id.0.clone();
        let turn = Turn {
            agent,
            person: person.clone(),
            conversation: place.conversation(),
            cause: place.change.clone(),
            asked: title,
            request,
            ask: Ask::Triggered(place.clone()),
            history: Vec::new(),
            invocation: Some(invocation.to_string()),
            typed: true,
            saved_by,
            private: triggered::alone_with(&event, &agent_id, &person),
            line,
            task,
        };
        let ask = triggered::answer_at(&turn.agent.id.0, &place);
        let Some(claim) = self.claim(workspace, &turn).await else {
            return Ok(crate::work::Started::Elsewhere(
                "another device takes this turn, or took it".to_owned(),
            ));
        };
        let going = loop {
            match self
                .going
                .enter_stopping(workspace, &turn, Arc::clone(&stop))
            {
                going::Entered::Began(going) => break going,
                going::Entered::Goes(going) => {
                    // What was written while the agent works there reaches the turn that goes,
                    // which answers it: this answer stands until that turn ended.
                    if going.reach_read(turn.clone(), self.going.reading()) == going::Handed::Taken
                    {
                        going.stopped_by(Arc::clone(&stop));
                        going.ended().await;
                        return Ok(crate::work::Started::Taken(Value::Null));
                    }
                    going.ended().await;
                }
                going::Entered::Closed => {
                    return Err("this device's agents are stopping".to_owned());
                }
            }
        };
        let (ended, used, failed) =
            Box::pin(self.answer_going(workspace, &turn, &going, claim)).await;
        if let Err(error) = &ended {
            tracing::warn!(%workspace, agent = %turn.agent.id.0, %error, "an agent's turn did not finish");
        }
        drop(going);
        // A turn that failed is not kept done: the shortcut's one retry asks the answer again
        // with the same cause, and this turn runs for that attempt. It fails with the reason
        // it could not go on, unless writing that to its reply failed first.
        if let Some(why) = failed {
            return Err(ended.err().unwrap_or(why));
        }
        let done = json!({"answered": ended.is_ok()});
        if let Err(error) = crate::work::keep_done(&self.workspaces, workspace, &ask, done).await {
            tracing::warn!(%workspace, %error, "an agent's turn is not kept as done");
        }
        Ok(crate::work::Started::Taken(used))
    }

    /// This device's claim on `turn` of `workspace`: none where another device takes it, or
    /// this one may not ([`crate::work::takes_turn`]). A triggered turn starts on the device
    /// whose trigger fired.
    async fn claim(&self, workspace: &str, turn: &Turn) -> Option<Arc<crate::work::Claim>> {
        let agent = &turn.agent.id.0;
        let Ask::Triggered(place) = &turn.ask;
        let ask = triggered::started_here(&self.workspaces, agent, place);
        match crate::work::takes_turn(&self.workspaces, workspace, &ask).await {
            Ok(claim) => Some(Arc::new(claim)),
            Err(not_taken) => {
                tracing::debug!(%workspace, %agent, %not_taken, "this device does not take an agent's turn");
                None
            }
        }
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
        // The person is the one whose root made the agent's key ([`agent_caller`]).
        let agent = self.agent_caller(workspace_id, &turn.agent, person).await?;
        // Every person who asked bounds the turn: whoever made the change, and the person who
        // saved the shortcut whose trigger fired, where that is someone else. What the turn
        // does there is allowed only where all of them, and the agent's own grants, allow it.
        let agent = match &turn.saved_by {
            Some(saver) => agent.agent_of(saver.clone()),
            None => agent,
        };
        let mut scope = TurnScope {
            provider: WorkspaceActionProvider::new(self.actions.clone(), workspace_id)
                .serving_ask(&turn.request),
            agent,
            workspace: workspace.to_owned(),
            workspace_id,
            rich_text: self
                .workspaces
                .tilolang()
                .map(|tilolang| tilolang.rich_text),
        };
        team::join(&self.workspaces, turn, &mut scope).await;
        across::join(&self.workspaces, self.placed.as_ref(), turn, &mut scope).await;
        // What the turn's tools reach beside the workspace's resources, each a provider of the
        // turn's own ([`TurnProvider`]): what its workspace keeps for the person who asked it
        // (their vault's uses by reference, mail, calendars, the services they connected), asked
        // of the device's dispatch, and a page's field filled from their vault.
        let kept = kept::TurnKept::for_turn(self, &scope.reply(turn)).await;
        let uses = vault_uses::TurnUses::for_turn(self, &scope.reply(turn)).await;
        scope.provider = scope.provider.answering(kept).answering(uses);
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
        // A moderation turn's read of a person's recorded changes, and its undo of them.
        scope.provider =
            scope
                .provider
                .answering(Arc::new(moderation_history::TurnHistory::for_turn(
                    self.workspaces.clone(),
                    workspace,
                    turn,
                )));
        Ok(scope)
    }

    /// Work `turn`, which holds its place as `going`: how it ended, what answered its model
    /// calls and what they used and cost, as the invocation that started it keeps them
    /// (`provider`, `model`, `tokens`, `cost_usd`), and why it failed where it may be asked
    /// again.
    async fn answer_going(
        &self,
        workspace: &str,
        turn: &Turn,
        going: &going::Going,
        claim: Arc<crate::work::Claim>,
    ) -> (Result<(), String>, Value, Option<String>) {
        let scope = match self.scope(workspace, turn).await {
            Ok(scope) => scope,
            // A turn whose scope could not be made failed: the same cause may run again.
            Err(error) => return (Err(error.clone()), Value::Null, Some(error)),
        };
        let mut turn = turn.clone();
        turn.history = scope.recall(&turn).await;
        // The person's stop of this run reaches the turn at its next step, and ends a model call
        // under way: it is the cancel of the invocation that began the turn, whose flag
        // [`going::Going`] holds.
        let mut reply = scope.reply(&turn);
        reply.claim = Some((claim, going.stop_flag()));
        Box::pin(self.answer_by_model(workspace, (scope, turn, reply), going)).await
    }

    /// The rest of [`Self::answer_going`] for a turn that the agent's model works: its scope,
    /// the turn and its reply, as `answer_going` made them.
    async fn answer_by_model(
        &self,
        workspace: &str,
        (scope, mut turn, mut reply): (TurnScope, Turn, Reply),
        going: &going::Going,
    ) -> (Result<(), String>, Value, Option<String>) {
        // A task is worked on over many agent sessions: the session is begun and its place
        // made before the model is asked, and one that may not begin ends here with its plain
        // line.
        let task = match Self::task_session(workspace, &mut turn, &reply).await {
            Ok(task) => task,
            Err(words) => {
                let ended = reply.finished(&Outcome::Stopped(words)).await;
                return (ended, Value::Null, None);
            }
        };
        let clock = tilo_clock::now();
        reply.taken().await;
        self.sub_began(&scope, &turn).await;
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
                // The day's limit and the task's own budget both bound the turn: the smaller.
                let day_left = limit.map(|limit| limit.saturating_sub(spent_before));
                let task_left = task.as_ref().and_then(|task| task.task.work.money_left());
                let budget = match (day_left, task_left) {
                    (Some(day), Some(task)) => Some(day.min(task)),
                    (day, task) => day.or(task),
                };
                let keeping = keeping.as_ref();
                let web = self.web(workspace);
                // What this server keeps of what was learned about the person, where the turn
                // is their own keeper's conversation with the agent: the newest facts, as data.
                let told = learned::learned_for(&self.workspaces, workspace, &turn)
                    .await
                    .map(|known| learned::told(&known));
                Box::pin(work(
                    model, &scope, &mut turn, budget, &mut reply, going, keeping, told, web,
                ))
                .await
            }
        };
        let outcome = match &task {
            Some(task) => task.ended_as(outcome, cost, clock.elapsed()).await,
            None => outcome,
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
        let failed = match &outcome {
            Outcome::Failed(why) => Some(why.clone()),
            Outcome::Answered(_) | Outcome::Stopped(_) | Outcome::Cancelled => None,
        };
        let ended = reply.finished(&outcome).await;
        self.sub_ended(&scope, &turn, &outcome).await;
        let summary = match &outcome {
            Outcome::Answered(_) => self.summary_of(&scope, &turn).await,
            Outcome::Stopped(_) | Outcome::Failed(_) | Outcome::Cancelled => None,
        };
        (
            ended,
            usage.as_invocation_keeps(cost, summary.as_deref()),
            failed,
        )
    }

    /// The summary `turn`'s conversation holds now, as its agent reads it: what the next
    /// turn there is told in place of the messages it covers. `None` for a turn that is no
    /// conversation's, or one whose conversation reads nothing.
    async fn summary_of(&self, scope: &TurnScope, turn: &Turn) -> Option<String> {
        let place = turn.ask.message()?;
        let reads = scope.provider.resource_reads()?;
        let caller = scope.agent.clone();
        let address = tilo_actions::remote::ResourceAddress {
            workspace: scope.workspace.clone(),
            app: CHAT.to_owned(),
            record: triggered::published(CHAT, &place.record),
        };
        let copy = reads.read(&caller, &address).await.ok()?;
        match copy {
            tilo_actions::remote::ResourceCopy::Read(read) => field(&read, "summary")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|summary| !summary.is_empty())
                .map(str::to_owned),
            _ => None,
        }
    }

    /// Begin an agent session on the task `turn` works on, as Tasks allows it: the session is
    /// said to have begun, the task's place is made when it has none, and `turn` is told where
    /// the task's working material is. None for a turn that is no task's.
    ///
    /// # Errors
    /// Tasks begins no session (its budget is used up, or its sessions were lost too often): the
    /// plain line, which is written on the task as its stop.
    async fn task_session(
        workspace: &str,
        turn: &mut Turn,
        reply: &Reply,
    ) -> Result<Option<task_work::TaskWork>, String> {
        let Some(task) = turn.task.clone() else {
            return Ok(None);
        };
        let work = task_work::TaskWork {
            service: reply.provider.clone(),
            caller: reply.caller.clone(),
            workspace: workspace.to_owned(),
            task,
        };
        work.begin().await?;
        let place = work.place().await;
        turn.request = task_work::with_place(&turn.request, &work.task.work, &place);
        Ok(Some(work))
    }

    /// The model `turn` is worked with, or why none works on it, in words its person reads: a
    /// call of the workspace's model that is under way ends once `stop` is set.
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
                    crate::workspaces::model_answers::Interaction::from_id(&sub.kind)
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
            let look_round = initiative::day_of_turn(
                &self.workspaces,
                &workspace_id.to_string(),
                &turn.person,
                &turn.cause,
                day,
            );
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
            workspaces.device_cert(),
            AGENTS_OWNER,
            &record,
            |doc| {
                let mut counting =
                    tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
                        .as_author(&keeper)
                        .in_app(AGENTS_OWNER, "Workspace");
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
    model: ModelAnswerer,
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

/// What answered a turn's calls and what they used.
#[derive(Clone, Debug, Default)]
struct TurnUsage {
    /// The provider and model of the last call, where a person's own model answered.
    answered: Option<ModelAnswered>,
    /// The tokens its calls counted: every input and output token.
    tokens: u64,
}

impl TurnUsage {
    /// What the `#answer` invocation keeps of the turn's model calls, by the names its record
    /// reads them under: what answered them, the tokens they took and what they cost in dollars.
    fn as_invocation_keeps(&self, cost_micros: u64, summary: Option<&str>) -> Value {
        let mut keeps = Map::new();
        if let Some(answered) = &self.answered {
            keeps.insert("provider".into(), json!(answered.provider));
            keeps.insert("model".into(), json!(answered.model));
        }
        keeps.insert("tokens".into(), json!(self.tokens));
        #[allow(
            clippy::cast_precision_loss,
            reason = "a cost in dollars is shown, never counted again"
        )]
        let usd = cost_micros as f64 / 1_000_000.0;
        keeps.insert("cost_usd".into(), json!(usd));
        // The conversation as this turn left it, where the turn kept one: the invocation that
        // took it is what keeps the summary per conversation and agent, and the next turn in
        // the conversation reads it in place of the messages it covers.
        if let Some(summary) = summary.filter(|summary| !summary.trim().is_empty()) {
            keeps.insert("summary".into(), json!(summary.trim()));
        }
        Value::Object(keeps)
    }
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
    workspace: String,
    workspace_id: WorkspaceId,
    /// What writes the rich text its record's actions take, where the program mounted a
    /// language runtime.
    rich_text: Option<Arc<dyn crate::tl_records::RichText>>,
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
        Reply {
            provider: self.provider.clone(),
            caller,
            workspace: self.workspace.clone(),
            ask: turn.ask.clone(),
            turn: turn.invocation.clone().unwrap_or_default(),
            message: Arc::default(),
            written: Written::default(),
            claim: None,
            rich_text: self.rich_text.clone(),
        }
    }

    /// What was said in `turn`'s conversation before the message that asked it, read as the
    /// turn's agent reads it: the conversation's summary, then its messages up to that one.
    /// Nothing for a task's turn, or where the conversation does not read.
    async fn recall(&self, turn: &Turn) -> Vec<Recalled> {
        let Some(place) = turn.ask.message() else {
            return Vec::new();
        };
        let Some(reads) = self.provider.resource_reads() else {
            return Vec::new();
        };
        let caller = self.agent.clone();
        let address = tilo_actions::remote::ResourceAddress {
            workspace: self.workspace.clone(),
            app: CHAT.to_owned(),
            record: triggered::published(CHAT, &place.record),
        };
        match reads.read(&caller, &address).await {
            Ok(tilo_actions::remote::ResourceCopy::Read(read)) => {
                recalled(&read, &place.item, &turn.agent.id.0)
            }
            Ok(_) => Vec::new(),
            Err(error) => {
                tracing::debug!(workspace = %self.workspace, %error, "a turn's conversation did not read");
                Vec::new()
            }
        }
    }
}

/// What `read`, a conversation of Chat as its core read answers it, says before the message
/// `asked`: its summary first, then the messages before that one, the last [`RECALLED`].
fn recalled(read: &Value, asked: &str, agent: &str) -> Vec<Recalled> {
    let mut out = Vec::new();
    if let Some(summary) = field(read, "summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
    {
        out.push(Recalled {
            by_agent: false,
            by: "What came before, in short".to_owned(),
            text: summary.to_owned(),
        });
    }
    let messages = field(read, "messages")
        .map(|messages| messages.get("items").unwrap_or(messages))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let named = |message: &Value| {
        ["$id", "id"]
            .iter()
            .filter_map(|key| message.get(*key).and_then(Value::as_str))
            .any(|id| id == asked || id.ends_with(&format!(":{asked}")))
    };
    let before = messages.iter().position(named).unwrap_or(messages.len());
    let from = before.saturating_sub(RECALLED);
    for message in messages.get(from..before).unwrap_or_default() {
        let mut words = Vec::new();
        if let Some(body) = message.get("body") {
            triggered::collect_text(body, &mut words);
        }
        let text = words.join("\n").trim().to_owned();
        if text.is_empty() {
            continue;
        }
        let who = message
            .pointer("/created/who")
            .cloned()
            .unwrap_or(Value::Null);
        let by_agent =
            who.get("agent").and_then(Value::as_str) == Some(agent) || who.as_str() == Some(agent);
        let by = who
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| who.as_str())
            .unwrap_or("Someone")
            .to_owned();
        out.push(Recalled { by_agent, by, text });
    }
    out
}

/// The field `name` of `read` or of the first object within it that has one.
fn field<'a>(read: &'a Value, name: &str) -> Option<&'a Value> {
    match read {
        Value::Object(fields) => fields
            .get(name)
            .or_else(|| fields.values().find_map(|inner| field(inner, name))),
        _ => None,
    }
}

/// Where a turn's answer is written: a message of Chat that answers the one that asked and
/// names the turn's invocation, or the agent's comment on the task.
struct Reply {
    provider: WorkspaceActionProvider,
    caller: Caller,
    workspace: String,
    ask: Ask,
    /// The `#answer` invocation the reply names as its `turn`, by its id.
    turn: String,
    /// The reply's message once Chat made it, by its published id.
    message: Arc<Mutex<Option<String>>>,
    /// What the model wrote into the reply so far ([`Written`]).
    written: Written,
    /// This device's claim on the turn, with the turn's stop: renewed with each write, and the
    /// turn stops once another device has it ([`Self::kept`]).
    claim: Option<(Arc<crate::work::Claim>, Arc<std::sync::atomic::AtomicBool>)>,
    /// What writes the rich text its record's actions take, where the program mounted a
    /// language runtime.
    rich_text: Option<Arc<dyn crate::tl_records::RichText>>,
}

/// What a model wrote into a reply while its turn went on: what it wrote in the rounds that
/// ended, and this round's words so far.
///
/// A round ends when its answer calls tools, or its call breaks off and is asked again. What it
/// wrote stays in the reply, and what the next round writes stands a paragraph below it.
#[derive(Clone, Default)]
struct Written {
    /// What the model wrote in the rounds that ended.
    before: Arc<Mutex<String>>,
    /// This round's words so far, and when the reply last took them.
    now: Arc<Mutex<(String, Option<Instant>)>>,
    /// What the model reasoned over the whole turn, as its provider shows it, and when the reply
    /// last took it.
    thought: Arc<Mutex<(String, Option<Instant>)>>,
}

impl Written {
    /// Take `piece`, more of what the model writes: the reply's body to write, once a moment
    /// passed since it last took any, and nothing before then.
    fn take(&self, piece: &str) -> Option<String> {
        let before = self.before.lock_recover();
        let mut now = self.now.lock_recover();
        now.0.push_str(piece);
        if !now.1.is_none_or(|last| last.elapsed() >= STREAM_EVERY) {
            return None;
        }
        let body = joined(before.trim(), now.0.trim());
        if body.is_empty() {
            return None;
        }
        now.1 = Some(tilo_clock::now());
        Some(body)
    }

    /// Take `piece`, more of what the model reasons: all its reasoning so far, once a moment
    /// passed since the reply last took any, and nothing before then.
    fn think(&self, piece: &str) -> Option<String> {
        let mut thought = self.thought.lock_recover();
        thought.0.push_str(piece);
        if !thought.1.is_none_or(|last| last.elapsed() >= STREAM_EVERY) {
            return None;
        }
        let reasoned = thought.0.trim().to_owned();
        if reasoned.is_empty() {
            return None;
        }
        thought.1 = Some(tilo_clock::now());
        Some(reasoned)
    }

    /// Everything the model reasoned so far.
    fn reasoned(&self) -> String {
        self.thought.lock_recover().0.trim().to_owned()
    }

    /// The round it writes in ended: what it wrote stays, below what came before it.
    fn ended(&self) {
        let mut before = self.before.lock_recover();
        let mut now = self.now.lock_recover();
        let said = now.0.trim();
        if !said.is_empty() {
            *before = joined(before.trim(), said);
        }
        now.0.clear();
        now.1 = None;
    }

    /// Everything the model wrote into the reply so far.
    fn wrote(&self) -> String {
        let before = self.before.lock_recover();
        let now = self.now.lock_recover();
        joined(before.trim(), now.0.trim())
    }

    /// What a finished answer says: the answer, below what its model wrote before it.
    fn answered(&self, answer: &str) -> String {
        let before = self.before.lock_recover();
        joined(before.trim(), answer.trim())
    }

    /// What a reply that ended before its answer says: what came, and below it why it ended.
    /// `why` is left out where the model already wrote it itself.
    fn kept(&self, why: &str) -> String {
        let wrote = self.wrote();
        let why = why.trim();
        if why.is_empty() || wrote.ends_with(why) {
            wrote
        } else if wrote.is_empty() {
            why.to_owned()
        } else {
            format!("{wrote}\n\n{why}")
        }
    }
}

/// `a` and `b` as one body, a blank line between them where both say something.
fn joined(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_owned(),
        (_, true) => a.to_owned(),
        _ => format!("{a}\n\n{b}"),
    }
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

    /// The route Chat's record of the workspace is reached at.
    fn chat(&self) -> DocumentRoute {
        DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: self.workspace.clone(),
            document: String::new(),
        }
    }

    /// Tell the person the turn is taken, where their ask shows it: the reply stands from then
    /// on, naming the turn, and takes what the turn's model writes while it writes.
    async fn taken(&self) {
        if self.ask.message().is_none() || self.message.lock_recover().is_some() {
            return;
        }
        if let Err(error) = self.send("").await {
            tracing::warn!(workspace = %self.workspace, %error, "the reply of an agent's turn was not begun");
        }
    }

    /// Send the reply to the message that asked, saying `words` and naming the turn: the
    /// message Chat made, by its published id.
    async fn send(&self, words: &str) -> Result<(), String> {
        self.kept().await?;
        let Some(place) = self.ask.message() else {
            return Err("the ask is no message of a conversation".to_owned());
        };
        let mut arguments = Map::new();
        arguments.insert("body".into(), json!(words));
        arguments.insert("reply_to".into(), place.reference());
        if !self.turn.is_empty() {
            arguments.insert("turn".into(), json!(self.turn));
        }
        let made = invoke_named(
            &self.provider,
            &self.caller,
            &self.chat(),
            &triggered::published(CHAT, &place.record),
            "send",
            arguments,
        )
        .await?;
        *self.message.lock_recover() = made_resource(&made);
        Ok(())
    }

    /// Say `words` in the reply: its message takes them in place of what it said, or the reply
    /// is sent with them where it was not yet.
    async fn say(&self, words: &str) -> Result<(), String> {
        let message = self.message.lock_recover().clone();
        let Some(message) = message else {
            return self.send(words).await;
        };
        self.kept().await?;
        let mut arguments = Map::new();
        arguments.insert("m".into(), json!(message));
        arguments.insert("body".into(), json!(words));
        // What the model reasoned stands beside the answer, whole each time: the reply's last
        // write carries all of it, however the writes between were spaced.
        let reasoned = self.written.reasoned();
        if !reasoned.is_empty() {
            arguments.insert("reasoning".into(), json!(reasoned));
        }
        if !self.turn.is_empty() {
            arguments.insert("turn".into(), json!(self.turn));
        }
        invoke_named(
            &self.provider,
            &self.caller,
            &self.chat(),
            &message,
            "edit",
            arguments,
        )
        .await
        .map(drop)
    }

    /// Take `piece`, more of what the model writes, into the reply: it is written there with
    /// what came before it once a moment passed since the reply last took any.
    async fn stream(&self, piece: &str) {
        if self.ask.message().is_none() {
            return;
        }
        let Some(words) = self.written.take(piece) else {
            return;
        };
        if let Err(error) = self.say(&words).await {
            tracing::debug!(workspace = %self.workspace, %error, "the reply did not take what its model wrote");
        }
    }

    /// Take `piece`, more of what the model reasons, into the reply beside what it wrote, once
    /// a moment passed since the reply last took any.
    async fn reason(&self, piece: &str) {
        if self.ask.message().is_none() || self.written.think(piece).is_none() {
            return;
        }
        if let Err(error) = self.say(&self.written.wrote()).await {
            tracing::debug!(workspace = %self.workspace, %error, "the reply did not take what its model reasoned");
        }
    }

    /// The model's call broke off and may be asked again: what it wrote stays in the reply, and
    /// what the next ask writes stands below it.
    fn broke_off(&self) {
        self.written.ended();
    }

    /// Leave this ask for what its person wrote next: its reply says the answer follows below,
    /// where the model wrote nothing into it yet.
    ///
    /// # Errors
    /// Chat did not take it.
    async fn went_on(&self) -> Result<(), String> {
        if self.ask.message().is_none() {
            return Ok(());
        }
        let said = self.written.wrote();
        if said.is_empty() {
            self.say(WENT_ON).await
        } else {
            self.say(&said).await
        }
    }

    /// Give the person the turn's answer: in its reply, or as a comment on its task. A reply
    /// that was stopped or broke off keeps what came and says below it why it ended; a finished
    /// answer stands below what its model wrote before it.
    ///
    /// # Errors
    /// Chat or Tasks did not take it.
    async fn finished(&self, outcome: &Outcome) -> Result<(), String> {
        let words = match outcome {
            Outcome::Answered(answer) => self.written.answered(answer),
            Outcome::Stopped(why) | Outcome::Failed(why) => self.written.kept(why),
            Outcome::Cancelled => self.written.kept("I was stopped."),
        };
        let words = if words.trim().is_empty() {
            "I finished without saying what I did; look at the history for my changes.".to_owned()
        } else {
            words
        };
        let Ask::Triggered(place) = &self.ask;
        if self.ask.message().is_some() {
            return self.say(&words).await;
        }
        self.kept().await?;
        triggered::write_answer(&self.provider, &self.caller, &self.workspace, place, &words).await
    }

    /// Send the view `source`, called `title`, into the reply's conversation through Chat's own
    /// `send_view`, as the agent acting for the person inside this turn.
    ///
    /// # Errors
    /// The turn answers in no conversation, or Chat refused it.
    async fn send_view(&self, title: &str, source: &str) -> Result<(), String> {
        let Some(place) = self.ask.message() else {
            return Err("only an answer in a conversation carries a view".to_owned());
        };
        self.kept().await?;
        let mut arguments = Map::new();
        arguments.insert("title".into(), json!(title));
        arguments.insert("definition".into(), json!(source));
        invoke_named(
            &self.provider,
            &self.caller,
            &self.chat(),
            &triggered::published(CHAT, &place.record),
            "send_view",
            arguments,
        )
        .await
        .map(drop)
    }

    /// Show `to`, called `label`, live in the reply's conversation: a message answering the one
    /// that asked, whose text refers to it. Each reader reads the reference with what they hold.
    ///
    /// # Errors
    /// The turn answers in no conversation, or Chat refused it.
    async fn show(&self, label: &str, to: &tilo_reference::Embed) -> Result<(), String> {
        let Some(place) = self.ask.message() else {
            return Err("only an answer in a conversation shows a document".to_owned());
        };
        let rich_text = self
            .rich_text
            .as_ref()
            .ok_or("this daemon runs no tilolang runtime: it writes no rich text")?;
        self.kept().await?;
        let body = rich_text.reference(label, to)?;
        let mut arguments = Map::new();
        arguments.insert("body".into(), body);
        arguments.insert("reply_to".into(), place.reference());
        invoke_named(
            &self.provider,
            &self.caller,
            &self.chat(),
            &triggered::published(CHAT, &place.record),
            "send",
            arguments,
        )
        .await
        .map(drop)
    }
}

/// The resource an action made, as its output names it.
fn made_resource(output: &Value) -> Option<String> {
    let made = output.get("resources")?.as_array()?.first()?;
    made.as_str()
        .or_else(|| made.get("id").and_then(Value::as_str))
        .map(str::to_owned)
}

/// Keep the agent `caller` works in `workspace`'s list of its agents: a computer's agent the
/// list does not name yet is added as it first arrives or acts, so the workspace's Agents tab
/// shows it. It is added once, under the name it came with; a name its keeper gives it later
/// stays. `person` is the agent's person, as this device admitted them: who writes the list.
pub async fn workspace_agent(
    workspaces: &Workspaces,
    workspace: WorkspaceId,
    caller: &Caller,
    person: &Caller,
) {
    let (Some(agent), Some(keeper)) = (
        caller.acting().agent_id(),
        caller.acting().acted_for_person(),
    ) else {
        return;
    };
    // Only the agent's own person lists it, as themselves.
    if !caller.is_device_agent() || person.acting().person_id() != Some(keeper) {
        return;
    }
    let workspace = workspace.to_string();
    if workspaces.agent_name(&workspace, agent).await.is_some() {
        return;
    }
    let (Some(user), Some(name)) = (caller.user_public_key(), caller.agent_name()) else {
        return;
    };
    if tilo_actions::remote::device_agent_id(user, name) == agent
        && let Err(error) = list_agent(workspaces, &workspace, (agent, name), person).await
    {
        tracing::warn!(%workspace, %agent, %error, "a computer's agent is not listed in the workspace's agents");
    }
}

/// Name the agent `agent` in `workspace`'s list as `name`, keeping its id: what the client the
/// agent serves named itself. The write is the record's own change, asked as `person` of the
/// workspace's governance and admitted by the record's grants as every change of theirs is, so
/// every later read of the list names it so. Nothing when the list does not name it, or names it
/// so already.
pub async fn rename_agent(
    workspaces: &Workspaces,
    workspace: WorkspaceId,
    (agent, name): (&str, &str),
    person: &Caller,
) -> Result<(), String> {
    let Some(current) = workspaces.agent_name(&workspace.to_string(), agent).await else {
        return Ok(());
    };
    let name = tilo_resource::NonBlank::parse(name.trim()).ok_or("the agent has no name")?;
    if current == name.as_str() {
        return Ok(());
    }
    let mut arguments = Map::new();
    arguments.insert("agent".into(), serde_json::json!(agent));
    arguments.insert("name".into(), serde_json::json!(name.as_str()));
    let mut change = Map::new();
    change.insert("action".into(), serde_json::json!("rename-agent"));
    change.insert("arguments".into(), Value::Object(arguments));
    keep_agents(
        workspaces,
        &workspace.to_string(),
        person,
        tilo_actions::remote::WORKSPACE_AGENT_UPDATE,
        change,
    )
    .await
    .map(drop)
}

/// Ask `operation` of `workspace`'s record of its agents with `arguments`, as `person`: through
/// this device's dispatch, where the record's grants decide it.
async fn keep_agents(
    workspaces: &Workspaces,
    workspace: &str,
    person: &Caller,
    operation: &str,
    arguments: Map<String, Value>,
) -> Result<Value, String> {
    invoke_at(
        workspaces.device_actions(),
        person,
        RemoteActionTarget::workspace_governance(workspace, operation),
        InvokeArgument::Json(arguments),
    )
    .await
}

/// Add the computer's agent `agent` named `name` to `workspace`'s list of its agents, kept by
/// `person`: the list starts with it where the workspace has none yet. The write is the record's
/// own change, asked as `person` of the workspace's governance and admitted by the record's
/// grants as every change of theirs is, so what follows the record sees it land and every later
/// read of the list names it. It is written on this device's copy as any member's write is, also
/// where another device (the person's server) holds the workspace and this one keeps a member
/// copy, and reaches that device from there. Nothing when another arrival added it first.
async fn list_agent(
    workspaces: &Workspaces,
    workspace: &str,
    (agent, name): (&str, &str),
    person: &Caller,
) -> Result<(), String> {
    let name = tilo_resource::NonBlank::parse(name.trim()).ok_or("the agent has no name")?;
    let mut arguments = Map::new();
    arguments.insert("name".into(), serde_json::json!(name.as_str()));
    arguments.insert("id".into(), serde_json::json!(agent));
    arguments.insert("mcp_client".into(), serde_json::json!(name.as_str()));
    match keep_agents(
        workspaces,
        workspace,
        person,
        tilo_actions::remote::WORKSPACE_AGENT_CREATE,
        arguments,
    )
    .await
    {
        // Another arrival listed it first: the workspace refused the second write, and the list
        // names the agent already, which is all this was for.
        Err(_) if workspaces.agent_name(workspace, agent).await.is_some() => Ok(()),
        other => other.map(drop),
    }
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
    invoke_at(
        Arc::new(provider.clone()),
        caller,
        target,
        InvokeArgument::Json(arguments),
    )
    .await
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
    provider: Arc<dyn ActionProvider + Send + Sync>,
    caller: &Caller,
    target: RemoteActionTarget,
    argument: InvokeArgument,
) -> Result<Value, String> {
    let request = invocation_of(target, argument);
    let caller = caller.clone();
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
    told: Option<String>,
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
    // How the turn answers, what it keeps about its person and the public web it may look up are
    // actions beside the agent tools, each told to the model as it publishes it.
    let mut own_tools = turn_tools::definitions(turn, keeping.is_some());
    own_tools.extend(web_search::definitions());
    // What was learned about the person, where the turn is their own keeper's conversation
    // with the agent, is told beside its instructions as data.
    let instructions = match told {
        Some(told) => format!("{}{}", instructions::for_turn(turn), told),
        None => instructions::for_turn(turn),
    };
    // What the turn is told: what came before in its conversation, its instructions with what
    // was learned about its person, its own tools beside the agent tools, and what it may spend.
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
        ..tilo_agent_turn::Asked::new(instructions, turn.request.clone())
    };
    let agent = turn.agent.id.0.clone();
    let usage = Arc::new(Mutex::new(TurnUsage::default()));
    let model = Arc::new(Mutex::new(Asking {
        usage: Arc::clone(&usage),
        turns: asking::again(turns, going.stop_flag()),
    }));
    let written = reply.written.clone();
    let answering = Answering {
        scope,
        going,
        keeping,
        at: tokio::sync::Mutex::new((turn, reply)),
        written,
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
    /// The ask the turn answers now, and where its answer is written.
    at: tokio::sync::Mutex<(&'a mut Turn, &'a mut Reply)>,
    /// What the model wrote into the reply, kept as the reply's own: a round that calls tools
    /// leaves what it wrote in the reply, and the answer stands below it.
    written: Written,
    /// The turn's agent tools, which a document it answers with is read through.
    server: &'a McpServer<WorkspaceActionProvider>,
    /// The public web the turn may look up in ([`web_search`]).
    web: &'a web_search::Web,
}

impl tilo_agent_turn::Doing<McpServer<WorkspaceActionProvider>> for Answering<'_> {
    fn stopped(&self) -> bool {
        self.going.stopped()
    }

    /// Each step is the turn invocation's own, which Chat shows beside the reply; what it
    /// costs is said in the log when it ends ([`steps`]).
    async fn carry_out<W>(
        &self,
        call: &ToolCall,
        step: tilo_agent_turn::Step,
        work: W,
    ) -> tilo_agent_turn::Answer
    where
        W: Future<Output = tilo_agent_turn::Answer> + Send,
    {
        // The round its answer calls tools in ended: what it wrote stays in the reply, and the
        // answer that follows stands below it.
        self.written.ended();
        let timed = steps::Step::begin(&call.name, (step.nth, step.of));
        let answer = Box::pin(timed.carried_out(work)).await;
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
        answer
    }

    /// The turn's own actions are carried out here ([`turn_tools`]), and the public web it looks
    /// up; every other tool is the agent tools'.
    async fn own(&self, call: &ToolCall) -> Option<tilo_agent_turn::Answer> {
        let (text, is_error) = if let Some(web) = self.web.tool(&call.name, &call.input).await {
            web
        } else {
            match turn_tools::call(&call.name, &call.input)? {
                Err(why) => (why, true),
                Ok(turn_tools::Call::View { title, source }) => {
                    let at = self.at.lock().await;
                    Box::pin(at.1.view(&title, &source)).await
                }
                Ok(turn_tools::Call::Document { path, title }) => {
                    let at = self.at.lock().await;
                    Box::pin(at.1.document(self.server, &path, title.as_deref())).await
                }
                Ok(turn_tools::Call::Learned(action)) => {
                    learned::invoke(self.keeping, action, &call.input).await
                }
            }
        };
        Some(tilo_agent_turn::Answer {
            text,
            pictures: Vec::new(),
            is_error,
        })
    }

    /// What the model writes, and the reasoning its provider shows, go into the reply as they
    /// come. The tools it calls are the steps of the turn's invocation, which Chat shows beside
    /// the reply.
    async fn hear(&self, mut hearing: tilo_agent_turn::Hearing) {
        let at = self.at.lock().await;
        while let Some(pieces) = hearing.next().await {
            for piece in pieces {
                match piece {
                    StreamEvent::TextDelta { text } => at.1.stream(&text).await,
                    StreamEvent::ReasoningDelta { text } => at.1.reason(&text).await,
                    // The call broke off and may be asked again: what comes then stands in
                    // its place.
                    StreamEvent::Failed { .. } => at.1.broke_off(),
                    _ => {}
                }
            }
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

/// Today, `YYYY-MM-DD`, as an agent's keeper counts the days its spending is charged to.
pub fn today() -> String {
    tilo_clock::local::today().to_string()
}

/// `micros` millionths of the currency, as a person reads an amount.
fn money(micros: u64) -> String {
    format!(
        "${}.{:02}",
        micros / 1_000_000,
        (micros % 1_000_000) / 10_000
    )
}

#[cfg(test)]
mod invocation_keeps_tests {
    use super::TurnUsage;

    #[test]
    fn a_turn_names_the_summary_it_kept_beside_its_cost() {
        let usage = TurnUsage {
            answered: None,
            tokens: 12,
        };
        let keeps = usage.as_invocation_keeps(250_000, Some("  what was said  "));
        assert_eq!(
            keeps.get("cost_usd"),
            Some(&serde_json::json!(0.25)),
            "the cost is named in dollars"
        );
        assert_eq!(
            keeps.get("summary"),
            Some(&serde_json::json!("what was said")),
            "the conversation's summary is named beside the cost"
        );
        assert!(
            usage.as_invocation_keeps(0, None).get("summary").is_none(),
            "a turn that kept none names none"
        );
        assert!(
            usage
                .as_invocation_keeps(0, Some("  "))
                .get("summary")
                .is_none(),
            "a blank summary is no summary"
        );
    }
}
