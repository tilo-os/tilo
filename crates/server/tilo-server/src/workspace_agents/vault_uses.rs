//! What a turn on this server uses of its person's vault, by reference.
//!
//! An agent's tools act on the workspace's action service, which knows the
//! workspace's resources and nothing of a person's own vault. The uses of a
//! vault item by reference (adding a mail account whose password the vault
//! keeps, keeping a provider's key from it) are the server's own to answer,
//! through its entry for a turn it carries out
//! ([`crate::workspaces::hosted_use`]). [`TurnUses`] is that entry as one
//! turn's tools reach it, a provider of the turn's own ([`TurnProvider`]): it
//! lists the uses the turn is offered beside the workspace's resources, and
//! takes them.
//!
//! No device of the person's is on, so the first use of an item somewhere
//! asks them. The question is Chat's own, of the kind `action`, put in the
//! conversation the agent works in as the turn's caller, and the tool call
//! ends at once: nothing waits on a thread. When the person answers, on any
//! device of theirs, the server reads the answer from the workspace's document
//! (`settle_uses`, at every look at what waits) and carries out what it kept
//! for that question itself: `allow` once, `always` with the item's leave
//! written first, `deny` not at all. It says `started` before it carries
//! anything out, so an answer is carried out at most once, and then how it
//! ended.
//!
//! A field of a web page is the one use whose secret leaves this server: the page is in the
//! workspace's Browser window, and only its engine writes into it. The turn names the field and
//! the item ([`person_vault::fill_row`]); the window is handed the fill and says which site its
//! page is at ([`crate::hosted_browser::HostedBrowsers::fill_for_turn`]); the entry decides for
//! that site ([`TurnUses::release`]), asking the person first as for every use. Their allow
//! fills nothing by itself: it lets that agent's next fill of that item at that site through.
//!
//! A secret the agent has no item for yet is asked for through the vault's own ask
//! ([`person_vault::ASK`]), the one way an agent comes by one: the tool call ends at once, and
//! this server asks the person on their own devices, in the system's protected password field
//! and in the system's words ([`person_vault::AskForItem::words`]). What they type goes into
//! their vault, and the agent is woken once in its conversation with them, told the item's
//! reference or that they declined. The ask is held by this process alone: a server that stops
//! while its person is asked asks nobody again, and the agent asks again when it is told to.
//!
//! Nothing here keeps a secret, and nothing here shows one. The arguments name the item by
//! its reference, the question shows it withheld, and the entry answers what the person would
//! be answered: the account, the provider. A text released for a page passes through here
//! once, on its way to be sealed to the window that fills it, in a type that prints nothing.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chat::question::QuestionState::{Answered, Settled, Waiting, Withdrawn};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tilo_actions::policy::MemberId;
use tilo_actions::remote::person_vault::{FillField, FillPage};
use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeArgument,
    InvokeRequest, Pending, RemoteActionTarget, person_vault,
};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_resource::Resource as _;
use tilo_workspace_agent::Agent;
use zeroize::Zeroizing;

use super::calendar_connect::{PutAsk, Typed, ask_for_password};
use super::turn_provider::{Ending, Rows};
use super::{
    Ask, CHAT, Reply, ServerAgents, Turn, TurnProvider, WRITE_WAIT, WorkspaceActionProvider,
    WorkspaceId, invoke_named, later,
};
use crate::hosted_browser::{Release, Releasing, TurnFill};
use crate::workspaces::hosted_use::{HostedTurn, OpenAsk, TurnRelease, TurnUse, UseAnswer};

/// What an agent is told when its person is asked first: nothing ran, and
/// it neither waits nor asks again.
const ASKED: &str = "Your person is asked in the conversation whether you may use this vault item \
     here. Nothing was used yet. It is used when they allow it there: tell them it waits for their \
     answer, and do not make this call again.";

/// The failure code of a use that was not made because its person is asked
/// first. It is not the code of an action that wants a confirmation: the
/// agent's tools word that one themselves, and the words here are the ones
/// the agent must read.
const ASKED_FIRST: &str = "asked_person_first";

/// What an agent is told when its person is asked first about a web call:
/// nothing was sent, and the same call goes through once they allowed it.
const CALL_ASKED: &str = "Your person is asked in the conversation whether you may make this call \
     with the vault item. Nothing was sent yet. Tell them it waits for their answer. Once they \
     allowed it, make the same call again with the same arguments: it is sent once.";

/// What the agent that asked for `target` is told when its person is asked
/// first.
fn asked_words(target: &RemoteActionTarget) -> &'static str {
    if *target == person_vault::call_target() {
        CALL_ASKED
    } else {
        ASKED
    }
}

/// What the question says of an answer that let one use through and carried
/// nothing out by itself: `agent` does it when it asks for the same again.
fn allowed_once(done: &Value, agent: &str) -> Option<String> {
    (done.get("allowed").and_then(Value::as_str) == Some("once"))
        .then(|| format!("Allowed once. It is done when {agent} next asks for the same."))
}

/// What the question says when the agent gave no reason of its own.
const NO_REASON: &str = "May I use this item from your vault here?";

/// What an agent is told when its person is asked first about a field of a page: nothing was
/// filled, and their allow fills nothing by itself.
const ASKED_PAGE: &str = "Your person is asked in the conversation whether you may fill this \
     vault item into this site's page. Nothing was filled yet, and their answer fills nothing by \
     itself: tell them it waits for their answer, and make this call again once they allowed it.";

/// Where a turn's question is put, and for which turn: kept with the ask,
/// so the server finds its way back after a restart.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct AskedIn {
    /// The conversation the turn is in, by its address in the workspace.
    conversation: String,
    /// The ask that caused the turn.
    cause: String,
    /// The agent session the turn's changes are grouped by.
    session: String,
}

/// What one turn uses of its person's vault. A turn's caller changes when
/// its person writes again while it works, so each use is taken as the caller
/// of the tool call that asked for it.
pub(super) struct TurnUses {
    agents: ServerAgents,
    workspace: String,
    workspace_id: WorkspaceId,
    agent: Agent,
    /// The person the turn acts for, by their person id.
    person: String,
    /// The ask the turn began with: its conversation is where a question goes.
    ask: Ask,
    /// The uses this turn is offered: none for a turn the entry refuses.
    offered: Vec<CatalogueRow>,
    /// The workspace's own actions, through which the question is put.
    service: WorkspaceActionProvider,
}

impl TurnUses {
    /// What `turn` uses of its person's vault, where `reply` holds what its
    /// tools begin as and act through.
    pub(super) async fn for_turn(agents: &ServerAgents, turn: &Turn, reply: &Reply) -> Arc<Self> {
        let workspace_id = reply.provider.workspace;
        let narrowed = narrowing(&turn.agent, &reply.workspace);
        let hosted = HostedTurn {
            workspace: workspace_id,
            caller: &reply.caller,
            keeper: &turn.agent.keeper,
            bound: narrowed.as_deref(),
        };
        let mut offered = agents.workspaces.uses_offered(&hosted).await;
        offered.extend(agents.workspaces.calls_offered(&hosted).await);
        if agents.workspaces.secret_ask_for_turn(&hosted).await.is_ok() {
            offered.push(person_vault::ask_row());
        }
        // A page's field is offered where this server places Browser windows, to a turn the
        // entry would not refuse before the vault is opened.
        if agents.browsers.is_some() && agents.workspaces.may_fill_pages(&hosted).await.is_ok() {
            offered.push(person_vault::fill_row());
        }
        Arc::new(Self {
            agents: agents.clone(),
            workspace: reply.workspace.clone(),
            workspace_id,
            agent: turn.agent.clone(),
            person: turn.person.clone(),
            ask: turn.ask.clone(),
            offered,
            service: reply.provider.clone(),
        })
    }

    /// Take the use `request` asks for as `caller`: how it ended, or the
    /// server's refusal.
    async fn take(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let invocation = request.invocation;
        let narrowed = narrowing(&self.agent, &self.workspace);
        let hosted = HostedTurn {
            workspace: self.workspace_id,
            caller,
            keeper: &self.agent.keeper,
            bound: narrowed.as_deref(),
        };
        let workspaces = &self.agents.workspaces;
        let taken = workspaces
            .use_for_turn(&hosted, request, asked_in(caller))
            .await?;
        let words = match taken {
            TurnUse::Done(value) => {
                return Ok(InvocationUpdate::Succeeded {
                    invocation,
                    output: ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                });
            }
            TurnUse::Waits => asked_words(&request.target).to_owned(),
            TurnUse::Asks(open) => match self.put(caller, &open).await {
                Ok(()) => asked_words(&request.target).to_owned(),
                Err(why) => {
                    workspaces.use_not_asked(&open);
                    format!(
                        "Your person could not be asked whether you may use this vault item \
                         ({why}), so nothing was used."
                    )
                }
            },
        };
        Ok(InvocationUpdate::Failed {
            invocation,
            error: asked(words),
        })
    }

    /// Have the field `request` names filled, in the page of this workspace's Browser window,
    /// from the vault item it names, as `caller`: how the fill ended, as the window said it,
    /// or the refusal. The window says which site its page is at, and [`Self::release`]
    /// decides for that site.
    async fn fill_page(
        self: &Arc<Self>,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let Some(browsers) = self.agents.browsers.clone() else {
            return Err(ActionCallError::Refused {
                reason: "This server keeps no Browser window, so it fills no page".to_owned(),
            });
        };
        let wanted: Option<FillField> = match &request.argument {
            InvokeArgument::Json(arguments) => {
                serde_json::from_value(Value::Object(arguments.clone())).ok()
            }
            _ => None,
        };
        let Some(wanted) = wanted else {
            return Err(ActionCallError::InvalidRequest {
                reason: "a field to fill names its element in the page's outline and the vault \
                         item to fill it from"
                    .to_owned(),
            });
        };
        // Refused before any window is asked, as before the vault is opened.
        let narrowed = narrowing(&self.agent, &self.workspace);
        let hosted = HostedTurn {
            workspace: self.workspace_id,
            caller,
            keeper: &self.agent.keeper,
            bound: narrowed.as_deref(),
        };
        self.agents.workspaces.may_fill_pages(&hosted).await?;
        let invocation = request.invocation;
        let fill = TurnFill {
            invocation,
            element: wanted.element.clone(),
            item: wanted.item.to_string(),
            reason: wanted.reason.trim().to_owned(),
        };
        let (this, by) = (Arc::clone(self), caller.clone());
        let releasing: Releasing = Box::new(move |origin| {
            let decided: Pin<Box<dyn Future<Output = Release> + Send>> =
                Box::pin(async move { this.release(&by, invocation, wanted, origin).await });
            decided
        });
        browsers
            .fill_for_turn(self.workspace_id, fill, releasing)
            .await
    }

    /// Decide the fill `invocation` of what `wanted` names, for the site `origin` its window
    /// says its page is at, as `caller`: the entry releases the item's text for that site, asks
    /// its person first, or refuses. A released text is handed on once, to be sealed to the
    /// window, and nothing else here holds it.
    async fn release(
        &self,
        caller: &Caller,
        invocation: InvocationId,
        wanted: FillField,
        origin: String,
    ) -> Release {
        let narrowed = narrowing(&self.agent, &self.workspace);
        let hosted = HostedTurn {
            workspace: self.workspace_id,
            caller,
            keeper: &self.agent.keeper,
            bound: narrowed.as_deref(),
        };
        let fill = FillPage {
            site: origin,
            item: wanted.item,
            reason: wanted.reason.trim().to_owned(),
        };
        let workspaces = &self.agents.workspaces;
        let released = workspaces
            .release_for_turn(&hosted, &fill, invocation, asked_in(caller))
            .await;
        match released {
            Ok(TurnRelease::Released(text)) => Release::Text {
                text: Zeroizing::new(text.text().to_owned()),
                into: text.field().as_str(),
            },
            Ok(TurnRelease::Waits) => Release::Asked(ASKED_PAGE.to_owned()),
            Ok(TurnRelease::Asks(open)) => match self.put(caller, &open).await {
                Ok(()) => Release::Asked(ASKED_PAGE.to_owned()),
                Err(why) => {
                    workspaces.use_not_asked(&open);
                    Release::Refused(format!(
                        "Your person could not be asked whether you may use this vault item \
                         ({why}), so nothing was filled."
                    ))
                }
            },
            Err(ActionCallError::Refused { reason }) => Release::Refused(reason),
            Err(error) => Release::Refused(error.to_string()),
        }
    }

    /// Put the question about `open` to the person, in the conversation
    /// the turn works in, as the turn's caller.
    async fn put(&self, caller: &Caller, open: &OpenAsk) -> Result<(), String> {
        let conversation = self.conversation().await?;
        let text = if open.reason.is_empty() {
            NO_REASON
        } else {
            open.reason.as_str()
        };
        let mut arguments = Map::new();
        arguments.insert("channel".into(), json!(conversation));
        arguments.insert("text".into(), json!(text));
        arguments.insert("kind".into(), json!(chat::question::ACTION));
        // "Always" is offered only for a use it may stand for.
        let mut answers = vec![json!({"id": chat::question::ALLOW})];
        if open.takes_always() {
            answers.push(json!({"id": chat::question::ALWAYS}));
        }
        answers.push(json!({"id": chat::question::DENY}));
        arguments.insert("answers".into(), Value::Array(answers));
        arguments.insert("detail".into(), open.detail.clone());
        let route = DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: self.workspace.clone(),
            document: String::new(),
        };
        invoke_named(
            &self.service,
            caller,
            &route,
            &collab_types::document::chain_resource_id(&chat::parts::chain()),
            "ask",
            arguments,
        )
        .await
        .map(drop)
    }

    /// The conversation the question goes to: the one the turn's ask was
    /// written in, or for an issue the person's own conversation with the
    /// agent.
    async fn conversation(&self) -> Result<String, String> {
        match &self.ask {
            Ask::Message { part, message, .. } => {
                let held = self
                    .agents
                    .read::<chat::parts::ChatMessages>(&self.workspace, CHAT, part)
                    .await?
                    .ok_or("the conversation is not held here")?;
                held.messages
                    .get(&tilo_resource::Id::from_raw(message.clone()))
                    .map(|asked| asked.channel.to_string())
                    .ok_or_else(|| "the message that asked is gone".to_owned())
            }
            Ask::Issue { .. } => Ok(chat::Chat::direct_id([
                MemberId::person(self.person.clone()),
                MemberId::agent(self.agent.id.0.clone()),
            ])
            .to_string()),
        }
    }
}

impl TurnProvider for TurnUses {
    /// The providers of the uses this turn is offered, as a listing of
    /// everything names them beside the workspace's resources.
    fn providers(&self, _caller: &Caller) -> Vec<CatalogueProvider> {
        let mut named: Vec<&str> = self
            .offered
            .iter()
            .map(|row| row.provider.as_str())
            .collect();
        named.dedup();
        named
            .into_iter()
            .map(|provider| CatalogueProvider {
                provider: provider.to_owned(),
                app_id: provider.to_owned(),
                workspace: self.workspace.clone(),
                document_id: String::new(),
                browsable: true,
            })
            .collect()
    }

    /// The rows `query` asks for, when it asks one of those providers.
    fn rows(&self, _caller: &Caller, query: &CatalogueQuery) -> Option<Rows> {
        let provider = query.provider.as_deref()?;
        let rows: Vec<CatalogueRow> = self
            .offered
            .iter()
            .filter(|row| row.provider == provider)
            .cloned()
            .collect();
        (!rows.is_empty()).then(|| Pending::ready(Ok(rows)))
    }

    /// A use by reference and the ask for a secret are this server's entry's to answer, also
    /// for a turn it offers none, which the entry then refuses.
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        person_vault::described_use(target).is_some() || *target == person_vault::ask_target()
    }

    /// A turn's caller changes when its person writes again while it works,
    /// so the use is taken as the caller of the tool call that asked for it.
    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move {
            if request.target == person_vault::page_target() {
                return self.fill_page(&caller, &request).await;
            }
            if request.target == person_vault::ask_target() {
                return self.ask_for_secret(&caller, &request).await;
            }
            self.take(&caller, &request).await
        })
    }
}

/// What an agent is told when its person is asked for a secret: nothing is kept yet, and it
/// neither waits nor asks again.
const SECRET_ASKED: &str = "Your person is asked on their own device for this, in a protected \
     field you cannot read. Nothing is kept yet. Tell them in one line to type it there. Once \
     they have, you are told here the reference the workspace's vault keeps it under. Do not make this call \
     again.";

/// What the agent is told its person did with an ask for a secret.
const SECRET_TYPED: &str = "they typed it where you cannot read it";
const SECRET_DECLINED: &str = "they declined to type it";
const SECRET_UNANSWERED: &str = "they were not asked, or did not answer in time";

/// An ask for a secret, held while its person is asked. Nothing in it is a secret.
struct SecretAsk {
    id: String,
    agent: String,
    name: String,
    person: String,
    workspace: String,
    asked: person_vault::AskForItem,
    asked_in: Value,
}

impl PutAsk for SecretAsk {
    fn id(&self) -> &str {
        &self.id
    }

    fn by_agent(&self) -> &str {
        &self.agent
    }

    fn agent_name(&self) -> &str {
        &self.name
    }

    fn of_person(&self) -> &str {
        &self.person
    }

    fn in_workspace(&self) -> &str {
        &self.workspace
    }

    fn item(&self) -> person_vault::AskForItem {
        self.asked.clone()
    }

    fn said(&self) -> String {
        self.asked.words(&self.name)
    }

    fn put_in(&self) -> &Value {
        &self.asked_in
    }
}

impl TurnUses {
    /// Ask the person `caller` acts for for the secret `request` names, on their own devices:
    /// the call ends at once, and the agent is woken with how they answered.
    async fn ask_for_secret(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let narrowed = narrowing(&self.agent, &self.workspace);
        let hosted = HostedTurn {
            workspace: self.workspace_id,
            caller,
            keeper: &self.agent.keeper,
            bound: narrowed.as_deref(),
        };
        let using = self.agents.workspaces.secret_ask_for_turn(&hosted).await?;
        let InvokeArgument::Json(arguments) = &request.argument else {
            return Err(ActionCallError::InvalidRequest {
                reason: "an ask for a secret names its kind and where it belongs".to_owned(),
            });
        };
        let asked: person_vault::AskForItem =
            serde_json::from_value(Value::Object(arguments.clone())).map_err(|_| {
                ActionCallError::InvalidRequest {
                    reason: "an ask for a secret names its kind (login, key, code-generator, \
                             ssh-key or value), belongsTo and, when known, username, and \
                             never the secret"
                        .to_owned(),
                }
            })?;
        let asked = asked
            .checked()
            .map_err(|reason| ActionCallError::InvalidRequest { reason })?;
        let open = SecretAsk {
            id: uuid::Uuid::new_v4().simple().to_string(),
            agent: using.agent,
            name: using.name,
            person: hex::encode(using.person),
            workspace: using.workspace,
            asked,
            asked_in: asked_in(caller),
        };
        let agents = self.agents.clone();
        tokio::spawn(async move { agents.ask_and_tell(open).await });
        Ok(InvocationUpdate::Failed {
            invocation: request.invocation,
            error: ActionFailure {
                code: ASKED_FIRST.to_owned(),
                message: SECRET_ASKED.to_owned(),
                details: None,
                retryable: false,
            },
        })
    }
}

impl ServerAgents {
    /// Ask the person `open` names for its secret, and wake the agent that asked once with how
    /// they answered: the reference the workspace's vault keeps it under, where they typed it.
    async fn ask_and_tell(&self, open: SecretAsk) {
        let asked = format!("Ask for {}", open.asked.belongs_to.join(" and "));
        let (answered, came) = match ask_for_password(&self.workspaces, &open).await {
            Typed::Kept(reference) => (
                SECRET_TYPED,
                format!(
                    "The workspace's vault keeps it as {reference}. Pass that reference where an action \
                     takes the secret: it is never the secret itself."
                ),
            ),
            Typed::Declined => (SECRET_DECLINED, "Nothing was kept.".to_owned()),
            Typed::Unanswered(why) => (
                SECRET_UNANSWERED,
                format!("Nothing was kept: {why}. Tell them so; ask again when they want."),
            ),
        };
        self.tell_of_password(&open, &asked, (answered, &came)).await;
    }
}

/// The rules a turn of `agent` is decided by in `workspace`, where anything
/// narrows it: what its entry bounds it to and, for a sub-agent, what keeps
/// it from whoever started it. `None` for an agent nothing narrows.
fn narrowing(agent: &Agent, workspace: &str) -> Option<Vec<tilo_action_policy::Rule>> {
    (agent.bound_rules().is_some() || agent.is_sub()).then(|| agent.turn_rules(workspace))
}

/// Where and for which turn `caller` asks, as an ask keeps it.
fn asked_in(caller: &Caller) -> Value {
    let turn = caller.turn();
    let kept = AskedIn {
        conversation: turn
            .map(|turn| turn.conversation.clone())
            .unwrap_or_default(),
        cause: turn.map(|turn| turn.message.clone()).unwrap_or_default(),
        session: caller
            .agent_session()
            .map(|session| session.id)
            .unwrap_or_default(),
    };
    serde_json::to_value(kept).unwrap_or(Value::Null)
}

/// The failure of a use that was not made because its person is asked:
/// `words` tell the agent so.
fn asked(words: String) -> ActionFailure {
    ActionFailure {
        code: ASKED_FIRST.to_owned(),
        message: words,
        details: None,
        retryable: false,
    }
}

/// The question this server put about `ask`, where the workspace's chat holds
/// it: the part's document, the message and the question. A question that
/// carries the ask's id and shows anything else than this server asked is not
/// it, and neither is one its agent did not write for that person.
fn question_about<'a>(
    parts: &'a [(String, chat::parts::ChatMessages)],
    ask: &OpenAsk,
) -> Option<(&'a str, String, &'a chat::question::Question)> {
    let (agent, person) = (
        MemberId::agent(ask.agent.clone()),
        MemberId::person(ask.person.clone()),
    );
    parts.iter().find_map(|(document, part)| {
        part.messages.iter().find_map(|(id, message)| {
            let question = message.question.as_ref()?;
            (question.kind.0 == chat::question::ACTION
                && message.author == agent
                && question.person == person
                && question.kept().as_ref() == Some(&ask.detail))
            .then(|| (document.as_str(), id.to_string(), question))
        })
    })
}

/// Whether the question about `ask` stands as this server asked it: one
/// message carries the ask, and it was never edited. An agent may edit a
/// message it wrote and may ask questions of its own, and neither is what its
/// person was asked.
fn as_asked(parts: &[(String, chat::parts::ChatMessages)], ask: &OpenAsk) -> bool {
    let (agent, person) = (
        MemberId::agent(ask.agent.clone()),
        MemberId::person(ask.person.clone()),
    );
    let mut carrying = parts
        .iter()
        .flat_map(|(_, part)| part.messages.iter())
        .filter(|(_, message)| {
            message.author == agent
                && message.question.as_ref().is_some_and(|question| {
                    question.kind.0 == chat::question::ACTION
                        && question.person == person
                        && question.kept().as_ref() == Some(&ask.detail)
                })
        });
    matches!(
        (carrying.next(), carrying.next()),
        (Some((_, message)), None) if !message.edited && message.history.is_empty()
    )
}

/// Say on the question `message` in the part `document` what came of its
/// answer.
pub(super) async fn settle(
    (service, asker): (&WorkspaceActionProvider, &Caller),
    workspace: &str,
    (document, message): (&str, &str),
    outcome: &str,
    told: Option<String>,
) -> Result<(), String> {
    let route = DocumentRoute {
        app_id: CHAT.to_owned(),
        cluster_id: workspace.to_owned(),
        document: document.to_owned(),
    };
    let mut arguments = Map::new();
    arguments.insert("message".into(), json!(message));
    arguments.insert("outcome".into(), json!(outcome));
    if let Some(told) = told {
        arguments.insert("detail".into(), json!(told));
    }
    let resource = format!("{}/messages/{message}", chat::parts::ChatMessages::KIND);
    // The answer was read from the workspace's own copy of the conversation, and the write goes
    // to the copy its asker acts on, which takes the person's answer in a moment later: until it
    // holds it, it refuses the settle as unanswered, and the settle is asked again.
    let deadline = later(WRITE_WAIT);
    let mut pause = Duration::from_millis(20);
    loop {
        let settled = invoke_named(
            service,
            asker,
            &route,
            &resource,
            "settle-question",
            arguments.clone(),
        )
        .await;
        match settled {
            Err(error)
                if error.contains(chat::question::NOT_ANSWERED) && Instant::now() < deadline =>
            {
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(Duration::from_millis(500));
            }
            settled => return settled.map(drop),
        }
    }
}

/// An answered question about a use, and who asked it.
struct AnsweredAsk {
    ask: OpenAsk,
    /// The part of the chat's messages that holds the question.
    document: String,
    message: String,
    answer: UseAnswer,
    /// The question's detail as the workspace's document holds it: what its
    /// person was shown when they answered.
    shown: Value,
    /// Whether their yes was given to a question that no longer stands as
    /// this server asked it: it is carried out as a no and ends as not done.
    changed: bool,
}

impl ServerAgents {
    /// Carry out what the people of `workspace` answered about the uses its
    /// agents asked for: each answer once, said to have started before it is
    /// carried out. Costs one look at what this server keeps when nothing waits.
    pub(super) async fn settle_uses(&self, workspace: &str) {
        let open = self.workspaces.open_use_asks(workspace);
        if open.is_empty() {
            return;
        }
        let Some(workspace_id) = WorkspaceId::parse(workspace) else {
            return;
        };
        let parts = self.chat_parts(workspace).await;
        let agents = self.agents(workspace).await.unwrap_or_default();
        for ask in open {
            if ask.started && !ask.interrupted {
                // Its answer is being carried out now.
                continue;
            }
            let Some((document, message, question)) = question_about(&parts, &ask) else {
                // Its question never reached the conversation: nothing to answer.
                if ask.interrupted {
                    self.workspaces.use_interrupted(&ask);
                }
                continue;
            };
            let shown = question.kept().unwrap_or(Value::Null);
            let answered = |answer| AnsweredAsk {
                ask: ask.clone(),
                document: document.to_owned(),
                message: message.clone(),
                answer,
                shown: shown.clone(),
                changed: false,
            };
            let answer = match question.state() {
                Waiting if !ask.interrupted => continue,
                Answered(given) if !ask.interrupted => match given.answer.0.as_str() {
                    chat::question::ALLOW => UseAnswer::Once,
                    chat::question::ALWAYS => UseAnswer::Always,
                    _ => UseAnswer::No,
                },
                // Taken back, or said to have ended by somebody else: nothing
                // is carried out for it any more.
                Withdrawn(_) | Settled(..) if !ask.interrupted => {
                    self.workspaces.use_not_asked(&ask);
                    continue;
                }
                // This server stopped while it carried the answer out: it is
                // not carried out a second time, and its person is told.
                _ => {
                    self.interrupted_use(workspace_id, &agents, answered(UseAnswer::No))
                        .await;
                    continue;
                }
            };
            let Some(agent) = agents.get(&ask.agent) else {
                // The agent no longer works here: nobody acts on the answer.
                self.workspaces.use_not_asked(&ask);
                continue;
            };
            // A yes to a question that was edited, or that stands beside another
            // carrying the same ask, was not given to what this server asked: it
            // is carried out as a no, so nothing is used and nothing stands.
            let changed = answer != UseAnswer::No && !as_asked(&parts, &ask);
            let answer = if changed { UseAnswer::No } else { answer };
            let carrying = AnsweredAsk {
                changed,
                ..answered(answer)
            };
            let (this, agent) = (self.clone(), agent.clone());
            let workspace = workspace.to_owned();
            tokio::spawn(async move {
                this.carry_out_use((&workspace, workspace_id), &agent, carrying)
                    .await;
            });
        }
    }

    /// Every part of the chat's messages `workspace` holds, by its document.
    pub(super) async fn chat_parts(
        &self,
        workspace: &str,
    ) -> Vec<(String, chat::parts::ChatMessages)> {
        let routes = self
            .workspaces
            .document_routes(workspace)
            .await
            .unwrap_or_default();
        let mut parts = Vec::new();
        for (app, document) in routes {
            if app != CHAT
                || collab_types::document::DocumentId::parse(&document)
                    .is_none_or(|part| chat::parts::part_index(&part).is_none())
            {
                continue;
            }
            if let Ok(Some(part)) = self
                .read::<chat::parts::ChatMessages>(workspace, CHAT, &document)
                .await
            {
                parts.push((document, part));
            }
        }
        parts
    }

    /// The agent that asked `ask`, acting for its person now: as the turn
    /// that asked, which is what the entry decides it by, and outside any
    /// turn, which is how it says what came of the answer once that turn
    /// has ended. `None` when the workspace admits the person on no device.
    async fn asker(
        &self,
        workspace: &str,
        workspace_id: WorkspaceId,
        ask: &OpenAsk,
        agent: &Agent,
    ) -> Option<(Caller, Caller)> {
        let mut person = [0; 32];
        hex::decode_to_slice(&ask.person, &mut person).ok()?;
        let device = self.workspaces.person_device(workspace_id, person).await?;
        let asked_in: AskedIn = serde_json::from_value(ask.asked_in.clone()).ok()?;
        let outside = Caller::authenticated(person, device)
            .in_workspace(workspace)
            .delegated_to(agent.id.0.clone())
            .named(&agent.name)
            .in_agent_session(asked_in.session);
        let turn = outside.clone().on_turn(
            tilo_action_policy::turn::Turn::new(asked_in.conversation, asked_in.cause),
            agent.bound_rules().unwrap_or_default(),
        );
        Some((turn, outside))
    }

    /// Carry out `answered`: say it started, let the entry carry out what it kept
    /// for the ask, and say how it ended.
    async fn carry_out_use(
        &self,
        (workspace, workspace_id): (&str, WorkspaceId),
        agent: &Agent,
        answered: AnsweredAsk,
    ) {
        let AnsweredAsk {
            ask,
            document,
            message,
            answer,
            shown,
            changed,
        } = answered;
        let Some((turn, outside)) = self.asker(workspace, workspace_id, &ask, agent).await else {
            // Its person is admitted here on no device any more.
            self.workspaces.use_not_asked(&ask);
            return;
        };
        let service = WorkspaceActionProvider::new(self.actions.clone(), workspace_id);
        let said = (&service, &outside);
        let at = (document.as_str(), message.as_str());
        // Claimed before anything is said or done: saying that it started
        // wakes the next look, which finds the ask taken and leaves it to
        // this one. Claimed by another look already, it is that one's.
        let Ok(claimed) = self.workspaces.claim_use(&ask.ask) else {
            return;
        };
        if answer != UseAnswer::No
            && let Err(error) = settle(said, workspace, at, chat::question::STARTED, None).await
        {
            // It cannot be said to have started (the question was taken back
            // meanwhile): nothing is carried out, and the ask is spent.
            tracing::warn!(%workspace, %error, "a use's question did not take that it started");
            self.workspaces.use_not_asked(&claimed);
            return;
        }
        let narrowed = narrowing(agent, workspace);
        let hosted = HostedTurn {
            workspace: workspace_id,
            caller: &turn,
            keeper: &agent.keeper,
            bound: narrowed.as_deref(),
        };
        let carried = self
            .workspaces
            .carry_out_use(&hosted, &claimed, &shown, answer)
            .await;
        // An allow that carried nothing out by itself says so on the question.
        let once = carried
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .and_then(|done| allowed_once(done, &agent.name));
        let (outcome, told) = match carried {
            _ if changed => (
                chat::question::FAILED,
                Some(super::asks::NOT_AS_ASKED.to_owned()),
            ),
            Ok(Some(_)) => (chat::question::DONE, None),
            Ok(None) => (chat::question::DROPPED, None),
            Err(error) => (chat::question::FAILED, Some(error.to_string())),
        };
        let told = told.or(once);
        if let Err(error) = settle(said, workspace, at, outcome, told).await {
            tracing::warn!(%workspace, %error, "a use's question did not take how it ended");
        }
    }

    /// The server stopped while it carried out the answer `answered` names:
    /// say that it could not be done, and keep it from running again.
    async fn interrupted_use(
        &self,
        workspace_id: WorkspaceId,
        agents: &BTreeMap<String, Agent>,
        answered: AnsweredAsk,
    ) {
        let AnsweredAsk {
            ask,
            document,
            message,
            ..
        } = answered;
        self.workspaces.use_interrupted(&ask);
        let Some(agent) = agents.get(&ask.agent) else {
            return;
        };
        let Some((_, outside)) = self.asker(&ask.workspace, workspace_id, &ask, agent).await else {
            return;
        };
        let service = WorkspaceActionProvider::new(self.actions.clone(), workspace_id);
        let told =
            "The server restarted while this was being done. Ask again if it is still needed.";
        if let Err(error) = settle(
            (&service, &outside),
            &ask.workspace,
            (document.as_str(), message.as_str()),
            chat::question::FAILED,
            Some(told.to_owned()),
        )
        .await
        {
            tracing::warn!(workspace = %ask.workspace, %error, "an interrupted use's question did not take how it ended");
        }
    }
}
