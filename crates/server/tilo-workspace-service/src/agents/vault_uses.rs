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
//! (`settle_asks`, at every look at what waits) and carries out what it kept
//! for that question itself: `allow` once, `always` with the item's leave
//! written first, `deny` not at all. It says `started` before it carries
//! anything out, so an answer is carried out at most once, and then how it
//! ended.
//!
//! A field of a web page is the one use whose secret leaves this server: the page is in the
//! workspace's Browser window, and only its engine writes into it. The turn names the field and
//! the item ([`person_vault::fill_row`]); the window is handed the fill and says which site its
//! page is at ([`super::windows::Windows::fill_for_turn`]); the entry decides for
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

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

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

use super::asks::{Answered, ChatAskKind, FoundQuestion, asked_in};
use super::calendar_connect::{PutAsk, Typed, ask_for_password};
use super::turn_provider::{Ending, Rows};
use super::windows::{Release, Releasing, TurnFill};
use super::{
    Ask, CHAT, Reply, ServerAgents, Turn, TurnProvider, WorkspaceActionProvider, WorkspaceId,
    invoke_named,
};
use crate::workspaces::Workspaces;
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
        let narrowed = turn.agent.bound_rules();
        let hosted = HostedTurn {
            workspace: workspace_id,
            caller: &reply.caller,
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
        let narrowed = self.agent.bound_rules();
        let hosted = HostedTurn {
            workspace: self.workspace_id,
            caller,
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
                tilo_failure::ignored!(
                    "arguments that name no field are refused below, saying what a fill needs",
                    serde_json::from_value(Value::Object(arguments.clone())).ok()
                )
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
        let narrowed = self.agent.bound_rules();
        let hosted = HostedTurn {
            workspace: self.workspace_id,
            caller,
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
        let narrowed = self.agent.bound_rules();
        let hosted = HostedTurn {
            workspace: self.workspace_id,
            caller,
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
            Ask::Triggered(_) => Ok(chat::Chat::direct_id([
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
        let narrowed = self.agent.bound_rules();
        let hosted = HostedTurn {
            workspace: self.workspace_id,
            caller,
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
        self.tell_of_password(&open, &asked, (answered, &came))
            .await;
    }
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
    invoke_named(
        service,
        asker,
        &route,
        &resource,
        "settle-question",
        arguments,
    )
    .await
    .map(drop)
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

impl ChatAskKind for OpenAsk {
    // Secret-use asks never expire for not yet appearing in Chat: a person's
    // later "allow" still carries out what was kept, and only an interrupted
    // ask is forgotten when its question is absent.
    const UNASKED_FOR: Option<Duration> = None;

    fn agent(&self) -> &str {
        &self.agent
    }

    fn asked_in(&self) -> &Value {
        &self.asked_in
    }

    fn asked_at_ms(&self) -> u64 {
        self.asked_at_ns / 1_000_000
    }

    fn expected_detail(&self) -> Value {
        self.detail.clone()
    }

    fn takes_always(&self) -> bool {
        // `TurnUses::put` only offers `ALWAYS` when `OpenAsk::takes_always()` is
        // true (not for a web call), so an unoffered `ALWAYS` answer in Chat is
        // refused as `No` and carries nothing out.
        Self::takes_always(self)
    }

    fn forget(&self, workspaces: &Workspaces) {
        workspaces.use_not_asked(self);
    }

    fn carry_out(
        self,
        agents: ServerAgents,
        (workspace, workspace_id): (String, WorkspaceId),
        agent: Agent,
        found: FoundQuestion,
        answer: Answered,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let answer = match answer {
            Answered::Once => UseAnswer::Once,
            Answered::Always => UseAnswer::Always,
            Answered::No => UseAnswer::No,
        };
        // A yes to a question that was edited, or that stands beside another
        // carrying the same ask, was not given to what this server asked: it
        // is carried out as a no, so nothing is used and nothing stands.
        let changed = answer != UseAnswer::No && (found.edited || !found.alone);
        let answer = if changed { UseAnswer::No } else { answer };
        let carrying = AnsweredAsk {
            ask: self,
            document: found.document,
            message: found.message,
            answer,
            shown: found.shown,
            changed,
        };
        Box::pin(async move {
            agents
                .carry_out_use((&workspace, workspace_id), &agent, carrying)
                .await;
        })
    }
}

impl ServerAgents {
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
        let Some((_, turn, outside)) = self
            .asker_of(workspace_id, &ask.person, &ask.asked_in, agent)
            .await
        else {
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
            // Deliberate fail-closed refusal for vault secret uses: if saying it
            // started fails (e.g. the question was taken back meanwhile), nothing is
            // carried out and the ask is spent rather than released for retry.
            tracing::warn!(%workspace, %error, "a use's question did not take that it started");
            self.workspaces.use_not_asked(&claimed);
            return;
        }
        let narrowed = agent.bound_rules();
        let hosted = HostedTurn {
            workspace: workspace_id,
            caller: &turn,
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
}
