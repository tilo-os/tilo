//! The agents a workspace's host runs, around the clock, for the workspaces it governs.
//!
//! Who they are is the workspace's own record of its agents (Assistant's
//! `tilo.workspace.agent`): every active agent made to run on the host ([`Agent::on_host`]) works
//! here, whoever keeps it, and none other. What they are asked is what the workspace's people
//! write: a message in a conversation the agent takes part in or that names it
//! ([`chat::summons`]), and an issue given to the agent
//! or written in once it has it ([`tasks_model::IssueAsk`]). A message asks too when something
//! else started the turn for a person (a reminder that is due, an event that fired) through
//! Chat's `ask-agent`: the agent then acts for the person its cause names ([`chat::cause`]). The
//! records hold every ask and every answer, so the host keeps nothing of its own: a host that
//! restarts finds the same asks.
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
use tilo_agent_session::workspace_actions::WorkspaceActions;
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

use crate::workspace_bots::events::{Authority, WorkspaceEvents};
use crate::workspace_bots::initiative::Initiative;
use crate::workspaces::Workspaces;
use crate::workspaces::person_model::{ModelRefusal, PersonModel, Spender};
use tilo_actions::remote::workspace_events;
use tilo_actions::remote::workspace_initiative;
use turn_provider::TurnProvider;

mod across;
mod answers;
mod asked;
mod asking;
mod asks;
mod calendar_connect;
mod calendars;
mod coding;
mod going;
mod instructions;
mod issue_work;
mod learned;
mod mail;
mod mail_connect;
mod shares;
mod steps;
mod sub_agents;
mod team;
mod turn_provider;
mod vault_uses;
mod written;

// What a turn reaches of the services its person connected.
mod connections;
// A turn's open of Browser, where this server places the window.
mod turn_browser;
// What a turn does at the page in that window.
mod turn_pages;

pub use asks::{Answered, Carried, Carries, ServerAsk};
pub use sub_agents::Line;

/// What a turn's reply says when the turn goes on with what its person wrote next.
const WENT_ON: &str = "I am going on with what you wrote next, and answer below.";
/// What the reply says of a turn that a server which stopped left begun.
const LEFT_BEGUN: &str = "I was interrupted while I worked on this: the server restarted after I \
                          had started. I did not start over, so that nothing is done twice. Ask \
                          again and I begin anew.";

/// What the reply says of an agent the workspace's member list does not hold yet: one made
/// before agents were members, whose keeper's device or an administrator's has not added it.
const NOT_A_MEMBER_YET: &str = "I am not a member of this workspace yet, so I cannot answer \
                                here. The person who keeps me, or an administrator, adds me \
                                when their device next opens the workspace.";

/// The app that keeps the conversations.
const CHAT: &str = "so.tilo.chat";
/// The app that keeps the workspace's list of its agents.
const AGENTS_OWNER: &str = tilo_workspace_agent::OWNER;
/// The app that keeps the issues.
const TASKS: &str = "so.tilo.tasks";
/// The longest the server goes without looking at every workspace, whatever is written in
/// between: what a missed wake costs.
const IDLE: Duration = Duration::from_secs(60);
/// The most times one look reads what a workspace owes: it reads again where a turn began or
/// went on under what it read ([`going::Reading`]).
const READINGS: usize = 3;
/// How long one model turn may take.
const TURN_TIME: Duration = Duration::from_secs(180);
/// The most tokens one model answer may take.
const ANSWER_TOKENS: u32 = 4096;
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
    actions: WorkspaceActions,
    roster_caller: Option<Caller>,
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
    pub fn new(actions: WorkspaceActions, workspace: WorkspaceId) -> Self {
        Self {
            actions,
            roster_caller: None,
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
    /// ([`WorkspaceActions::landed`]).
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

    fn workspace_caller<'a>(&self, person_work: &'a Caller) -> std::borrow::Cow<'a, Caller> {
        let Some(roster) = &self.roster_caller else {
            return std::borrow::Cow::Borrowed(person_work);
        };
        // MCP owns the turn's execution. Carry that same lease when the
        // workspace call switches to the roster device.
        let roster = roster.clone();
        std::borrow::Cow::Owned(match person_work.execution_lease() {
            Some(execution) => roster.in_execution(execution),
            None => roster,
        })
    }

    fn serve(&self, caller: &Caller, message: RemoteActionMessage) -> Pending<RemoteActionMessage> {
        // What a tool call asks of the workspace's action service is counted for its step.
        let asking = steps::asking(&message);
        let caller = match &message {
            RemoteActionMessage::Invoke(request)
                if !tilo_actions::remote::is_ask(&request.target) =>
            {
                self.workspace_caller(caller)
            }
            RemoteActionMessage::Cancel { invocation }
                if self.actions.observe(caller, invocation).is_none() =>
            {
                self.workspace_caller(caller)
            }
            _ => std::borrow::Cow::Borrowed(caller),
        };
        let answer = self
            .actions
            .serve(self.workspace, &caller, message)
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
        runtime.spawn(async move {
            completer.complete(this.page(&caller, query).await);
        });
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
            .or_else(|| self.actions.observe(&self.workspace_caller(caller), id))
            .or_else(|| {
                self.roster_caller
                    .as_ref()
                    .and_then(|_| self.actions.observe(caller, id))
            })
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
    fn changed(&self, caller: &Caller, scope: WatchScope) -> Option<Pending<()>> {
        // Either answer is the same to a watch: what it follows is read again, and a refusal is
        // told by that read. A service that answers nothing at all is not running, and the
        // watch waits its own deadline instead of on a wake that would come at once for ever.
        self.actions
            .serve(
                self.workspace,
                caller,
                RemoteActionMessage::ChangedRequest {
                    correlation: 0,
                    scope,
                },
            )
            .map(|answer| answer.map(|_| ()))
    }

    /// One document of the workspace, read as every reader of it is answered: the same read a
    /// person's device answers, from what the workspace's service holds.
    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        let reads: Arc<dyn tilo_actions::remote::ResourceReads> = Arc::new(self.actions.clone());
        Some(reads)
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
/// it ([`RemoteActionMessage::SearchRequest`]): each resource by what its app tells search
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
        self.serve(
            caller,
            RemoteActionMessage::SearchRequest {
                correlation: 0,
                needle: needle.to_owned(),
                within: within.clone(),
            },
        )
        .map(|answer| match answer {
            RemoteActionMessage::SearchResponse { result, .. } => result,
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

/// Whether a write to `app`'s records may ask the agents here something: Chat's, the tracker's,
/// the record of the workspace's agents, and a record asks are kept in ([`asked::RECORDS`]).
fn wakes(app: &str) -> bool {
    app == CHAT || app == TASKS || app == AGENTS_OWNER || asked::keeps_asks(app)
}

/// What carries one turn's model calls, made fresh for each turn on its own thread: a test's
/// script, standing where a workspace's model would ([`ServerAgents::for_test`]).
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
    /// An ask of the agent package, kept in a tilolang app's record: the answer is the ask's
    /// reply ([`asked`]).
    Asked {
        /// The app whose record keeps the ask.
        app: String,
        /// The ask, by its id in that record, and its type.
        ask: String,
        ty: String,
        /// Which time it is asked: its first reply is 1.
        attempt: u32,
        /// Whether what it is about is the asker's and the agent's alone: every member who may
        /// read it is one of the two ([`tilo_workspace_service::asks::private_to`]). Only
        /// there is a turn told and does it keep what its agent learns about its keeper
        /// ([`learned`]).
        private: bool,
    },
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
        let began = Instant::now();
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
}

/// What the first step of a look read of a workspace: what it owes its agents, and where the
/// server's turns stood when the reading began. The look's second step enters with it.
pub struct LookRead {
    reading: going::Reading,
    asks: Asks,
}

/// The agents a host runs for the workspaces it governs.
#[derive(Clone)]
pub struct ServerAgents {
    workspaces: Workspaces,
    actions: WorkspaceActions,
    roster: Option<(
        crate::agents::ServerAgentLink,
        tilo_agent_session::AgentSessions,
    )>,
    /// A test's script, answering in a workspace that linked no model: `None` outside tests,
    /// where such a workspace is told what to link.
    model: Option<Model>,
    /// The turns going now: one for each agent in each conversation, with what reaches it
    /// while it works.
    going: Arc<going::Turns>,
    /// The coding agents worked here: the sub-agents whose work a coding harness does in
    /// their workspace's machine ([`coding`]). `None` works none.
    coding: Option<Arc<coding::Coders>>,
    /// The events this host runs for its people, which a turn's tools reach for its person.
    events: Option<WorkspaceEvents>,
    /// What carries out the answers to what code of this server asked.
    carriers: asks::Carriers,
    /// What the people's assistants find to do on their own, which a turn's tools reach too.
    initiative: Option<Initiative>,
    /// The Browser windows this server places for its workspaces, where it places any: a
    /// turn's open of Browser has its window placed there ([`turn_browser`]), and a turn has
    /// a field of its workspace's page filled there from its person's vault.
    browsers: Option<Arc<crate::hosted_browser::HostedBrowsers>>,
    /// The devices this server placed itself for its workspaces' windows, where it was told
    /// them: none of them is a device a read for a person acts as ([`across`]).
    placed: Option<crate::environment::EnvironmentRegistry>,
}

impl ServerAgents {
    /// Run the workspace roster through the host's approved signing sessions.
    #[must_use]
    pub fn with_sessions(
        workspaces: Workspaces,
        actions: WorkspaceActions,
        server: &crate::Server,
        sessions: tilo_agent_session::AgentSessions,
    ) -> Self {
        Self::build(
            workspaces,
            actions,
            None,
            Some((server.agent_link(), sessions)),
        )
    }

    /// A test's script answers in every workspace here that linked no model.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn scripted(mut self, model: Model) -> Self {
        self.model = Some(model);
        self
    }

    /// Exercise turn machinery in fixtures that do not provision peer sessions.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn for_test(
        workspaces: Workspaces,
        actions: WorkspaceActions,
        model: Option<Model>,
    ) -> Self {
        Self::build(workspaces, actions, model, None)
    }

    fn build(
        workspaces: Workspaces,
        actions: WorkspaceActions,
        model: Option<Model>,
        roster: Option<(
            crate::agents::ServerAgentLink,
            tilo_agent_session::AgentSessions,
        )>,
    ) -> Self {
        let agents = Self {
            workspaces,
            actions,
            roster,
            model,
            going: Arc::default(),
            coding: None,
            events: None,
            carriers: Arc::default(),
            initiative: None,
            browsers: None,
            placed: None,
        };
        // What a person answers about a connection of theirs is carried out from the start.
        agents.carries(connections::WORD, connections::answers(&agents.workspaces));
        // What a turn asks its person is put to them in its conversation.
        agents.ask_in_conversations();
        agents.carry_sends();
        agents
    }

    async fn roster_device(
        &self,
        workspace: WorkspaceId,
        agent: &Agent,
        keeper: [u8; 32],
    ) -> Result<Option<[u8; 32]>, String> {
        let Some((server, sessions)) = &self.roster else {
            return Ok(None);
        };
        let denied =
            || "the hosted agent has no approved signing session for this workspace".to_owned();
        let device = server
            .roster_session_device(sessions, workspace, &agent.id.0, &keeper)
            .await
            .filter(|device| {
                sessions.matches_roster(device, workspace, &agent.id.0, &keeper, agent.created_at)
            })
            .ok_or_else(denied)?;
        Ok(Some(device))
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
    pub fn placing(mut self, browsers: Option<Arc<crate::hosted_browser::HostedBrowsers>>) -> Self {
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

    /// Look at every governed workspace now, then again whenever a member writes to a record an
    /// ask or an agent is kept in, and at every one of them once in each [`IDLE`] whatever was
    /// written, until the host stops.
    pub async fn run(self) {
        let mut writes = self.workspaces.document_writes();
        let mut due: BTreeSet<String> = self
            .workspaces
            .list()
            .await
            .into_iter()
            .map(|summary| summary.workspace)
            .collect();
        let mut whole_at = next_whole();
        loop {
            for workspace in std::mem::take(&mut due) {
                if let Err(error) = self.look(&workspace).await {
                    tracing::debug!(%workspace, %error, "the host's agents did not look at a workspace");
                }
            }
            match woke(&mut writes, whole_at).await {
                Woke::Write(write) => {
                    if wakes(&write.app) {
                        due.insert(write.workspace);
                    }
                }
                Woke::Whole => {
                    due.extend(
                        self.workspaces
                            .list()
                            .await
                            .into_iter()
                            .map(|summary| summary.workspace),
                    );
                    whole_at = next_whole();
                }
                Woke::Closed => return,
            }
            // A burst of writes (a person typing, an agent's own steps) is read as one look.
            while let Ok(write) = writes.try_recv() {
                if wakes(&write.app) {
                    due.insert(write.workspace);
                }
            }
        }
    }

    /// Start every turn `workspace` owes its host's agents that is not going yet.
    ///
    /// # Errors
    /// The workspace's records could not be read.
    pub async fn look(&self, workspace: &str) -> Result<(), String> {
        // What a person answered about a use of their vault is carried out
        // whether or not anything asks an agent now.
        Box::pin(self.settle_uses(workspace)).await;
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
        Ok(self.enter_past(workspace, read, past))
    }

    /// The first step of a look: what `workspace` owes, read once what its people answered and
    /// what its coding agents said is settled. [`Self::enter_look`] acts on it. A look takes
    /// both steps at once ([`Self::look`]); whoever must stand between the two, as a test of
    /// what a look does with a reading that a turn has overtaken, takes them one at a time.
    ///
    /// # Errors
    /// What the workspace owes could not be read.
    pub async fn read_look(&self, workspace: &str) -> Result<LookRead, String> {
        // What its people answered about what its agents asked them first.
        self.settle_asks(workspace).await;
        // What a coding agent's harness said, and a stop for it, with no turn of it going.
        if self.codes_in(workspace) {
            Box::pin(self.settle_coding(workspace)).await;
        }
        // Taken before anything is read: a turn that begins or goes on from here may answer
        // something newer than what this look reads.
        let reading = self.going.reading();
        let asks = self.asks(workspace).await?;
        Ok(LookRead { reading, asks })
    }

    /// The second step of a look: begin every turn `read` holds whose place is free, and hand
    /// the others to the turns that go there. Whether what was read was still current wherever
    /// it was handed on: where it was not, a look reads again.
    pub fn enter_look(&self, workspace: &str, read: LookRead) -> bool {
        self.enter_past(workspace, read, None)
    }

    /// [`Self::enter_look`], leaving out the ask `past` names ([`Self::look_past`]).
    fn enter_past(
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
                        let agent = turn.agent.id.0.clone();
                        // Boxed, as in `answer`: the task holds a pointer to the turn and
                        // carries none of it in a future of its own.
                        let (ended_on, ended) =
                            Box::pin(agents.answer_going(&workspace, &turn, &held)).await;
                        if let Err(error) = ended {
                            tracing::warn!(%workspace, %agent, %error, "a host agent's turn did not finish");
                        }
                        drop(held);
                        agents.look_after(workspace, (agent, ended_on)).await;
                    });
                }
            }
        }
        current
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
        let mut routes = self
            .workspaces
            .document_routes(workspace)
            .await
            .map_err(|error| error.to_string())?;
        if !routes.iter().any(|(app, _)| app == TASKS) && self.found_tracker(workspace).await {
            routes = self
                .workspaces
                .document_routes(workspace)
                .await
                .map_err(|error| error.to_string())?;
        }
        let mut asks = self.chat_asks(workspace, &agents, &routes).await?;
        // What the list holds for the sub-agents worked here is asked of them first, so this
        // look reads those asks with every other.
        let roster = self.team_roster(workspace).await?;
        if let Some(roster) = &roster {
            self.ask_team(workspace, roster, &agents).await?;
        }
        let (owed, stopped) =
            asked::turns(&self.workspaces, workspace, &agents, roster.as_ref()).await?;
        asks.turns.extend(owed);
        asks.stopped.extend(stopped);
        asks.turns.extend(
            self.issue_turns(workspace, &agents, &routes, now_ms)
                .await?,
        );
        Ok(asks)
    }

    /// Found the tracker of `workspace`, which keeps none: nobody opened Tasks in it yet. This
    /// server keeps an agent at work there, and an agent is given work as an issue of the
    /// workspace's tracker: without one its person can give it none from a device that shows no
    /// Tasks. The server founds it as it founds any app's document a member's first window
    /// opens: from the app's own factory in its trusted catalogue, as the workspace's governor.
    /// Whether it stands now.
    async fn found_tracker(&self, workspace: &str) -> bool {
        let catalogue = tilo_agent_session::AppCatalogue::native();
        let tracker = crate::workspace_bots::workspace_resource(&catalogue, TASKS);
        let founded = self
            .workspaces
            .prepare_application(
                workspace,
                self.workspaces.governor(),
                &catalogue,
                TASKS,
                &tracker,
            )
            .await;
        if let Err(error) = &founded {
            tracing::warn!(%workspace, %error, "the workspace's tracker could not be founded: its agents can be given no issue");
        }
        founded.is_ok()
    }

    /// What `workspace`'s Rust Chat conversations still hold for `agents`: the questions they
    /// asked that wait, the answers that owe a turn, and the team's sub-agents. Turns are asks of
    /// the agent package ([`asked`]), never read from these conversations.
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
        if self
            .read::<chat::chat::Chat>(workspace, CHAT, "")
            .await?
            .is_none()
        {
            self.workspaces.note_waiting(workspace, waiting);
            return Ok(asks);
        }
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
        }
        self.workspaces.note_waiting(workspace, waiting);
        answered.retain(|wake| !woken.contains(&wake.key));
        asks.answered = answered;
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
            let part = crate::workspace_bots::opened::<tasks::issue_list::IssueList>(&mut doc)?;
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
        let roster: BTreeMap<String, Agent> = agents
            .agents
            .iter()
            // A sub-agent worked elsewhere is its own runner's, but for a coding agent where
            // coding agents are kept ([`coding`]).
            .filter(|agent| {
                agent.is_active()
                    && agent.on_host
                    && agent.mcp_client.is_none()
                    && (sub_agents::worked_here(agent) || self.codes(agent))
            })
            .map(|agent| (agent.id.0.clone(), agent.clone()))
            .collect();
        self.add_members(workspace, &roster).await;
        Ok(roster)
    }

    /// Add to the workspace's member list each agent of `roster` it does not hold yet, acting
    /// for its keeper, as a member write for the keeper this server signs: an agent made before
    /// agents were members becomes one once, where the keeper's right and this server's allow
    /// it. One they do not answers that it is not a member yet ([`NOT_A_MEMBER_YET`]) until its
    /// keeper's device adds it.
    async fn add_members(&self, workspace: &str, roster: &BTreeMap<String, Agent>) {
        let Some(policy) = self.workspaces.policy_of(workspace).await else {
            return;
        };
        for (id, agent) in roster {
            if policy.agents.contains_key(id) {
                continue;
            }
            let Some(keeper) = keeper_of(agent) else {
                continue;
            };
            if let Err(error) = self
                .workspaces
                .add_agent(
                    workspace,
                    tilo_action_policy::MemberId::person(keeper.clone()),
                    id,
                    &keeper,
                )
                .await
            {
                tracing::debug!(%workspace, agent = %id, %error, "an agent is not a member here yet");
            }
        }
    }

    /// Whether the workspace's member list holds `agent`.
    async fn is_member(&self, workspace: &str, agent: &str) -> bool {
        self.workspaces
            .policy_of(workspace)
            .await
            .is_some_and(|policy| policy.agents.contains_key(agent))
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
    /// message that is owed no answer any more is not begun ([`Self::owed_now`]).
    ///
    /// # Errors
    /// The person is not admitted here on any device, or the answer could not be written.
    pub async fn answer(&self, workspace: &str, turn: &Turn) -> Result<(), String> {
        match self.going.enter(workspace, turn) {
            going::Entered::Began(held) => {
                let (_, ended) = Box::pin(self.answer_going(workspace, turn, &held)).await;
                drop(held);
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
        let device = self
            .workspaces
            .work_device(
                workspace_id,
                person,
                &tilo_action_policy::MemberId::agent(turn.agent.id.0.clone()),
            )
            .await
            .ok_or("the person who asked is admitted here on no device")?;
        let keeper = tilo_action_policy::PersonId::parse(&turn.agent.keeper)
            .ok()
            .and_then(|keeper| keeper.root())
            .ok_or("the hosted agent has no keeper")?;
        let roster = self
            .roster_device(workspace_id, &turn.agent, keeper)
            .await?
            .map(|device| (keeper, device, turn.agent.created_at));
        let mut scope = TurnScope {
            provider: WorkspaceActionProvider::new(self.actions.clone(), workspace_id)
                .offering(self.events.clone())
                .offering_initiative(self.initiative.clone())
                .serving_ask(&turn.request),
            person: Caller::authenticated(person, device).in_workspace(workspace),
            roster,
            workspace: workspace.to_owned(),
            workspace_id,
            replying: asked::Replying::of(&self.workspaces, workspace, turn),
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
        let ended = reply.finished(outcome).await;
        self.sub_ended(&acting, turn, outcome, false).await;
        ended
    }

    /// Whether a host that stopped left `turn` begun: its reply shows a step taken, and no turn
    /// of this host is on it, since this one only begins now. Words that turn's model wrote
    /// into the reply stay there, and `reply` goes on below them ([`written`]).
    async fn left_begun(&self, workspace: &str, turn: &Turn, reply: &Reply) -> bool {
        if let Some(replying) = &reply.replying {
            let begun = replying.left_begun().await;
            if begun {
                reply.written.goes_on();
            }
            return begun;
        }
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
            Ok(Some(held)) => chat::summons::left_begun(&held, message, &turn.agent.id.0, *attempt),
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
    async fn answer_going(
        &self,
        workspace: &str,
        turn: &Turn,
        going: &going::Going,
    ) -> (Ask, Result<(), String>) {
        // The place is this turn's own from here on, and nothing else answers in it: what the
        // workspace owes now is what begins.
        let turn = match self.owed_now(workspace, turn).await {
            Ok(Some(owed)) => owed,
            Ok(None) => return (turn.ask.clone(), Ok(())),
            Err(error) => return (turn.ask.clone(), Err(error)),
        };
        let scope = match self.scope(workspace, &turn).await {
            Ok(scope) => scope,
            Err(error) => return (turn.ask.clone(), Err(error)),
        };
        let began = turn.ask.clone();
        let reply = scope.reply(&turn);
        if self.left_begun(workspace, &turn, &reply).await {
            // Its steps may have changed things already, and nothing says which: starting over
            // would do them twice. It ends here, and asking again is the person's to do.
            let left = Outcome::Failed(LEFT_BEGUN.to_owned());
            let ended = reply.finished(&left).await;
            crate::workspace_telling::turn_ended(
                &self.workspaces,
                self.events.as_ref(),
                &scope.person,
                workspace,
                &turn,
                &left,
            )
            .await;
            self.sub_ended(&scope, &turn, &left, false).await;
            return (turn.ask, ended);
        }
        // An ask's turn is this server's only once its claim holds: another device of the
        // workspace that claimed it first answers it instead.
        if let Some(replying) = &reply.replying {
            match replying.take().await {
                Ok(true) => {}
                Ok(false) => return (turn.ask, Ok(())),
                Err(error) => return (turn.ask, Err(error)),
            }
        }
        if matches!(turn.ask, Ask::Asked { .. })
            && !self.is_member(workspace, &turn.agent.id.0).await
        {
            let refused = Outcome::Failed(NOT_A_MEMBER_YET.to_owned());
            return (turn.ask, reply.finished(&refused).await);
        }
        if self.codes(&turn.agent) {
            // Its harness in the workspace's machine does the work, not the model's loop: its
            // turn leaves here, before anything is begun that only that loop ends.
            let (ended_on, ended) =
                Box::pin(self.answer_coding(&scope, (turn, reply), going)).await;
            return (ended_on, ended);
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
    ) -> (Ask, Result<(), String>) {
        // An issue is worked on over many agent sessions: its place is made and the session
        // counted before the model is asked, and one that may not begin ends here with its
        // plain line.
        let task = match self.issue_session(workspace, &mut turn, &reply).await {
            Ok(task) => task,
            Err(words) => {
                let ended = reply.finished(&Outcome::Stopped(words)).await;
                return (turn.ask, ended);
            }
        };
        let clock = Instant::now();
        reply.taken().await;
        self.sub_began(workspace, &turn).await;
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
                let task_left = task.as_ref().and_then(|(_, before)| before.money_left());
                let budget = match (day_left, task_left) {
                    (Some(day), Some(task)) => Some(day.min(task)),
                    (day, task) => day.or(task),
                };
                let keeping = keeping.as_ref();
                work(model, &scope, &mut turn, budget, &mut reply, going, keeping).await
            }
        };
        let outcome = match &task {
            Some((task, before)) => task.ended_as(outcome, cost, clock.elapsed(), before).await,
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
        let ended = reply.finished_with(&outcome, cost, &usage).await;
        crate::workspace_telling::turn_ended(
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
        (turn.ask, ended)
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
            Ask::Message { .. } | Ask::Asked { .. } => return Ok(None),
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

    /// The model `turn` is worked with, or why none works on it, in words its person reads,
    /// for a turn its person may stop: a call of the workspace's model that is under way ends
    /// once `stop` is set.
    ///
    /// The workspace pays, so its own model answers: the one it linked for work its agents do.
    /// Once it linked a provider its settings decide alone, and a choice that cannot be used is
    /// said, never replaced by a model somebody else pays for. A workspace that linked nothing is
    /// told what to link.
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
        let linked = tokio::task::spawn_blocking(move || asked.linked())
            .await
            .map_err(|error| error.to_string())?
            .map(|linked| (theirs, linked.model.id));
        match linked {
            Ok((theirs, name)) => Ok(Arc::new(move || {
                Some(Box::new(WorkspaceTurns {
                    model: theirs.clone(),
                    name: name.clone(),
                    stop: Arc::clone(&stop),
                    answered: None,
                }) as Box<dyn PlanTurns>)
            })),
            Err(refused @ ModelRefusal::NoModelLinked { .. }) => {
                self.model.clone().ok_or_else(|| refused.words(None))
            }
            Err(refused) => Err(refused.words(None)),
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
pub(crate) async fn count_spend(
    workspaces: &Workspaces,
    workspace: &str,
    agent: &Agent,
    day: &str,
    cost: u64,
) -> Result<(), String> {
    let keeper = agent.keeper.clone();
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
            .map_err(|refused| refused.model_error())?;
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
    /// The person, as their own device is admitted here.
    person: Caller,
    roster: Option<([u8; 32], [u8; 32], i64)>,
    workspace: String,
    workspace_id: WorkspaceId,
    /// Where the reply of a turn made from an ask is written ([`asked`]).
    replying: Option<asked::Replying>,
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
        let in_session = |caller: Caller| {
            caller
                .named(&turn.agent.name)
                .in_agent_session(&session)
                .asked(&turn.asked)
        };
        let caller = in_session(self.person.clone().delegated_to(turn.agent.id.0.clone()))
            .on_turn(cause.clone(), turn.agent.turn_rules(&self.workspace));
        let mut provider = self.provider.clone();
        provider.roster_caller = self.roster.map(|(keeper, device, created_at)| {
            in_session(
                Caller::delegated_agent(turn.agent.id.0.clone(), keeper, device)
                    .in_workspace(&self.workspace),
            )
            .on_roster_turn(cause, created_at)
        });
        Reply {
            provider,
            caller,
            workspace: self.workspace.clone(),
            ask: turn.ask.clone(),
            written: written::Written::of(turn),
            shows_in: shares::shows_in(turn),
            replying: self.replying.clone(),
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
    /// Where the reply of a turn made from an ask is written.
    replying: Option<asked::Replying>,
}

impl Reply {
    /// Tell the person the turn is taken, where their ask shows it. The reply is there from
    /// then on, and takes what the turn's model writes while it writes ([`written`]).
    async fn taken(&self) {
        if let Ask::Message { .. } = &self.ask {
            let _ = self.work(json!({"state": "taken"}), None).await;
        }
    }

    /// Write `words` at the end of the reply while the turn works: what a coding agent's harness
    /// says on the way.
    ///
    /// # Errors
    /// The turn answers no ask, or its reply did not take them.
    async fn says(&self, words: &str) -> Result<(), String> {
        match &self.replying {
            Some(replying) => match words.trim() {
                "" => Ok(()),
                words => replying.say(&format!("{words}\n\n")).await,
            },
            None => Err("only the reply to an ask takes words on the way".to_owned()),
        }
    }

    /// Tell the person what the agent is doing now. A note Chat does not take costs the person
    /// the step alone, and [`Self::work`] says why.
    async fn doing(&self, did: &str) {
        if let Some(replying) = &self.replying {
            replying.step(did, "", false).await;
        }
        if let Ask::Message { .. } = &self.ask {
            let _ = self.work(json!({"state": "doing", "did": did}), None).await;
        }
    }

    /// Tell the person what the agent is doing now, and wait for nothing: the note is handed
    /// to the workspace's action service, which takes a workspace's requests in the order
    /// they came. So it stands on the reply before whatever the turn writes there next, and
    /// no tool waits until Chat has taken it. Whether the turn has a reply that takes notes.
    fn notes(&self, did: &str) -> bool {
        if let Some(replying) = self.replying.clone() {
            let did = did.to_owned();
            tokio::spawn(async move { replying.step(&did, "", false).await });
            return true;
        }
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
        if let Some(replying) = &self.replying {
            return replying
                .finish(
                    &self.written.below(WENT_ON),
                    tilo_workspace_service::asks::AskState::Done,
                )
                .await;
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
        if let Some(replying) = &self.replying {
            let (words, state) = self.written.ending(outcome);
            return replying.finish(&words, state).await;
        }
        match &self.ask {
            Ask::Asked { .. } => Err("the ask's reply is not open here".to_owned()),
            Ask::Message { .. } => {
                let (state, words) = match outcome {
                    Outcome::Answered(words) => ("provider_complete", words.as_str()),
                    Outcome::Stopped(words) => ("provider_truncated", words.as_str()),
                    Outcome::Failed(words) => ("provider_failed", words.as_str()),
                    Outcome::Cancelled => ("provider_cancelled", ""),
                };
                let mut work = json!({"state": state});
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
                match self.work(work, Some(words)).await {
                    Ok(()) => Ok(()),
                    Err(error) => Err(self.unfinished(&error).await),
                }
            }
            Ask::Issue { record, issue, .. } => {
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

/// The person `agent` acts for, by their user root, as the workspace's member list names
/// people: its keeper.
pub(super) fn keeper_of(agent: &Agent) -> Option<String> {
    tilo_action_policy::PersonId::parse(&agent.keeper)
        .ok()
        .and_then(|keeper| keeper.root())
        .map(hex::encode)
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
    let document = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    let name = tilo_resource::NonBlank::parse(name.trim()).ok_or("the agent has no name")?;
    let (agent, person) = (agent.to_owned(), person.to_owned());
    let (listed, keeper) = (agent.clone(), person.clone());
    workspaces
        .author_workspace_document(
            workspace,
            workspaces.governor(),
            (AGENTS_OWNER, &document),
            |doc| {
                // A workspace whose people made no agent yet has no list of them: it starts here.
                let agents = if doc.get_heads().is_empty() {
                    tilo_resource::Replica::<WorkspaceAgents>::new()
                } else {
                    tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
                };
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
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    // Its membership: the workspace's member list holds it, acting for its person.
    workspaces
        .add_agent(
            workspace,
            tilo_action_policy::MemberId::person(keeper.clone()),
            &listed,
            &keeper,
        )
        .await
        .map_err(|error| error.to_string())
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
                    if Instant::now() > deadline {
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
    // What the turn keeps about its keeper, and in a conversation the view it may answer with,
    // are its own tools beside the agent tools.
    let mut own_tools = learned::with_tools(Vec::new(), keeping);
    own_tools.extend([view_tool(turn), shares::tool(turn)].into_iter().flatten());
    let asked = tilo_agent_turn::Asked {
        history: instructions::history(&turn.history),
        own_tools,
        sees: turns.sees(),
        budget,
        most_rounds: None,
        output_tokens: ANSWER_TOKENS,
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
    /// What a turn that is its keeper's own keeps of what it learns ([`learned`]).
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
            at.1.doing(&call.name).await;
            timed.noted();
        }
        // The view tool writes on the reply itself.
        drop(at);
        let answer = Box::pin(timed.carried_out(work)).await;
        {
            let mut noted = self.noted.lock_recover();
            if noting == steps::Note::After {
                noted.1 = Some(call.name.clone());
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

    /// What the turn keeps about its keeper is carried out here, and the view or document it
    /// answers with;
    /// every other tool is the agent tools'.
    async fn own(&self, call: &ToolCall) -> Option<tilo_agent_turn::Answer> {
        let (text, is_error) = if call.name == ANSWER_WITH_VIEW {
            let at = self.at.lock().await;
            Box::pin(at.1.view(&call.input)).await
        } else if call.name == shares::ANSWER_WITH_DOCUMENT {
            let at = self.at.lock().await;
            Box::pin(at.1.document(self.server, &call.input)).await
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
    let now = Instant::now();
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

/// Today, `YYYY-MM-DD`, as an agent's keeper counts the days its spending is charged to.
pub(crate) fn today() -> String {
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

/// What ended the wait between two looks.
enum Woke<T> {
    /// A member wrote this.
    Write(T),
    /// Every workspace is due: the time for it came, or writes were missed.
    Whole,
    /// Nobody is left to write.
    Closed,
}

/// When every workspace is looked at next, counted from now.
fn next_whole() -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    now.checked_add(IDLE).unwrap_or(now)
}

/// Wait for the next of `writes`, and no longer than until `whole_at`, when every workspace is
/// looked at again. That time is moved on only by a look at all of them and never by a write,
/// and it is read before the writes are: on a server where something is written all the time, as
/// on a silent one, a turn whose wake was missed is found within [`IDLE`] and one pass over the
/// workspaces.
async fn woke<T: Clone + Send>(
    writes: &mut tokio::sync::broadcast::Receiver<T>,
    whole_at: tokio::time::Instant,
) -> Woke<T> {
    if tokio::time::Instant::now() >= whole_at {
        return Woke::Whole;
    }
    match tokio::time::timeout_at(whole_at, writes.recv()).await {
        Ok(Ok(write)) => Woke::Write(write),
        Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) | Err(_) => Woke::Whole,
        Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => Woke::Closed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server where something is written all the time still looks at every workspace once in
    /// each [`IDLE`]: a write that is there before each wait begins never puts that look off, and
    /// no write is lost to it. With nothing written the wait ends when the time has come, and
    /// with nobody left to write it says so.
    #[tokio::test(start_paused = true)]
    async fn every_workspace_is_looked_at_in_time_whatever_is_written() {
        let written = tokio::sync::broadcast::Sender::new(8);
        let mut writes = written.subscribe();
        let began = tokio::time::Instant::now();
        let mut whole_at = next_whole();
        let mut wholes = Vec::new();
        let mut taken = Vec::new();
        // Five minutes with a write every ten seconds.
        for _ in 0..30 {
            assert!(written.send(()).is_ok(), "the wait reads what is written");
            tokio::time::advance(Duration::from_secs(10)).await;
            loop {
                match woke(&mut writes, whole_at).await {
                    Woke::Write(()) => {
                        taken.push(began.elapsed());
                        break;
                    }
                    Woke::Whole => {
                        wholes.push(began.elapsed());
                        whole_at = next_whole();
                    }
                    Woke::Closed => break,
                }
            }
        }
        assert_eq!(
            taken.len(),
            30,
            "no write is lost to a look at every workspace"
        );
        assert_eq!(
            wholes,
            [60, 120, 180, 240, 300].map(Duration::from_secs),
            "every workspace is looked at once a minute while the writes keep coming"
        );

        let silent = tokio::time::Instant::now();
        assert!(matches!(woke(&mut writes, next_whole()).await, Woke::Whole));
        assert_eq!(
            silent.elapsed(),
            IDLE,
            "with nothing written, when its time came"
        );
        drop(written);
        assert!(matches!(
            woke(&mut writes, next_whole()).await,
            Woke::Closed
        ));
    }
}
