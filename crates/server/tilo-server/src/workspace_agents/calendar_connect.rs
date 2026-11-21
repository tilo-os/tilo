//! A calendar its person tells their assistant of, connected on this server.
//!
//! An agent's tools act on the workspace's action service, which knows the workspace's
//! resources and nothing of a person's own calendars. Connecting one is the server's own to
//! answer, through its entry for a turn it carries out
//! ([`crate::workspaces::Workspaces::connect_calendar_for_turn`]): the turn is its keeper's
//! own. [`TurnConnects`] is that entry as one turn's tools reach it, a provider of the turn's
//! own ([`TurnProvider`]): it lists the one action beside the workspace's resources, and takes
//! it. The action takes what the person said (the calendar server's address, the user name
//! there, a name for the account) and nothing else.
//!
//! The password is its person's to type where no agent reads it. So the tool call ends at once,
//! told that they are asked, and this puts the question to them: Chat's own, of the kind that
//! shows a protected field, in the words the server's entry says. It goes into the person's own
//! conversation with the agent, whatever conversation the turn works in, because it shows where
//! their calendar is and under which name they sign in. What they type goes from their device
//! to their vault, and the question is answered with the item's reference.
//!
//! The server reads that answer from the workspace's document at its next look
//! (`settle_connects`), before the look reads which answers wake their askers: it claims the
//! ask and says on the question that it started, so an answer is carried out at most once and
//! wakes nobody by itself. Then, on a task of its own, the entry signs in to the calendar
//! server, keeps the account for reading only and names the agent for it; the question is told
//! how it ended, and the agent is woken once with what came of it: the calendars by their
//! names, or the calendar server's refusal. Neither the password nor the item's reference
//! passes a model on this way.
//!
//! A question that no longer says what the server asked (an agent may edit a message it wrote,
//! and may ask questions of its own) connects nothing, and what was typed under it is not kept.

use std::sync::Arc;
use std::time::Duration;

use chat::question::QuestionState::{Answered, Settled, Waiting, Withdrawn};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tilo_actions::policy::MemberId;
use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeRequest,
    Pending, RemoteActionTarget, person_accounts,
};
use tilo_actions::{ActionFailure, InvocationUpdate};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_workspace_agent::Agent;

use super::mail::narrowing;
use super::turn_provider::{Ending, Rows};
use super::vault_uses::settle;
use super::{CHAT, Reply, Turn, TurnProvider, WorkspaceActionProvider, WorkspaceId, invoke_named};
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::{
    CALENDAR_CONNECT, CALENDARS, ConnectAsk, Connected, HostedTurn, TurnConnect,
};

/// What an agent is told when its person is asked for the password: nothing is connected yet,
/// and it neither waits nor asks again.
const ASKED: &str = "Your person is asked in your conversation with them for the password of \
     this calendar account, in a protected field you cannot read. Nothing is connected yet. Tell \
     them to type it there. Once they have, the account is connected for reading only and you \
     are told which calendars it has: do not make this call again.";

/// What an agent is told when its person was asked for this account's password before and has
/// not answered.
const WAITS: &str = "Your person was asked for the password of this calendar account before, in \
     a protected field of your conversation with them, and has not typed it yet. Nothing is \
     connected yet. Tell them it waits for them there, and do not make this call again.";

/// The failure code of a call that connected nothing because its person is asked first. It is
/// not the code of an action that wants a confirmation: the agent's tools word that one
/// themselves, and the words here are the ones the agent must read.
pub(super) const ASKED_FIRST: &str = "asked_person_first";

/// How long an ask whose question is not in the conversation is kept: its turn's server stopped
/// between keeping it and asking.
const UNASKED_FOR: Duration = Duration::from_secs(300);

/// What a question is told when this server stopped while it connected the calendar.
const INTERRUPTED: &str = "The server restarted while this calendar was being connected. Tell \
     your assistant again if it is still wanted.";

/// What the agent is told its person did, where they typed, and where they declined.
pub(super) const TYPED: &str = "they typed the password where you cannot read it";
pub(super) const DECLINED: &str = "they declined to type the password";

/// The most of the calendars' names a question's outcome and an agent's news carry, in bytes.
const MOST_NAMES_BYTES: usize = 600;

/// Where a turn's question is put, and for which turn: kept with the ask, so the server finds
/// its way back after a restart.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct AskedIn {
    /// The conversation the turn is in, by its address in the workspace.
    conversation: String,
    /// The ask that caused the turn.
    cause: String,
    /// The agent session the turn's changes are grouped by.
    session: String,
}

/// Where and for which turn `caller` asks, as an ask keeps it.
pub(super) fn asked_in(caller: &Caller) -> Value {
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

/// Whoever carries out a workspace's agents: what reads the answers to the questions this
/// server put itself, and carries them out.
pub(super) type Carrier = super::ServerAgents;

/// What this server keeps of a question it put itself for a person's password: enough to put
/// it, to find it again, and to tell whether it still says what was asked.
pub(super) trait PutAsk: Sync {
    /// The agent that asked, by its id.
    fn by_agent(&self) -> &str;
    /// The person asked: their root key, hex.
    fn of_person(&self) -> &str;
    /// What the protected field is drawn from.
    fn shown(&self) -> Value;
    /// The question's words, as this server says them.
    fn said(&self) -> String;
    /// Where and for which turn it was asked.
    fn put_in(&self) -> &Value;
}

impl PutAsk for ConnectAsk {
    fn by_agent(&self) -> &str {
        &self.agent
    }

    fn of_person(&self) -> &str {
        &self.person
    }

    fn shown(&self) -> Value {
        self.detail()
    }

    fn said(&self) -> String {
        self.words()
    }

    fn put_in(&self) -> &Value {
        &self.asked_in
    }
}

/// Put the question `open` keeps to its person as `caller`, in their own conversation with the
/// agent: what it shows of their account is theirs alone to read.
pub(super) async fn put(
    service: &WorkspaceActionProvider,
    caller: &Caller,
    workspace: &str,
    open: &dyn PutAsk,
) -> Result<(), String> {
    let conversation = chat::Chat::direct_id([
        MemberId::person(open.of_person().to_owned()),
        MemberId::agent(open.by_agent().to_owned()),
    ])
    .to_string();
    let mut arguments = Map::new();
    arguments.insert("channel".into(), json!(conversation));
    arguments.insert("text".into(), json!(open.said()));
    arguments.insert("kind".into(), json!(chat::question::SECRET));
    arguments.insert("detail".into(), open.shown());
    let route = DocumentRoute {
        app_id: CHAT.to_owned(),
        cluster_id: workspace.to_owned(),
        document: String::new(),
    };
    invoke_named(
        service,
        caller,
        &route,
        &collab_types::document::chain_resource_id(&chat::parts::chain()),
        "ask",
        arguments,
    )
    .await
    .map(drop)
}

/// What one turn connects of its person's calendars. A turn's caller changes when its person
/// writes again while it works, so each telling is taken as the caller of the tool call that
/// made it.
pub(super) struct TurnConnects {
    workspaces: Workspaces,
    workspace: String,
    workspace_id: WorkspaceId,
    agent: Agent,
    /// The connecting this turn is offered: none for a turn the entry refuses.
    offered: Vec<CatalogueRow>,
    /// The workspace's own actions, through which the question is put.
    service: WorkspaceActionProvider,
}

impl TurnConnects {
    /// What `turn` connects of its person's calendars, where `reply` holds what its tools begin
    /// as and act through.
    pub(super) async fn for_turn(workspaces: &Workspaces, turn: &Turn, reply: &Reply) -> Arc<Self> {
        let workspace_id = reply.provider.workspace;
        let narrowed = narrowing(&turn.agent, &reply.workspace);
        let hosted = HostedTurn {
            workspace: workspace_id,
            caller: &reply.caller,
            keeper: &turn.agent.keeper,
            bound: narrowed.as_deref(),
        };
        let offered = workspaces.calendar_connect_offered(&hosted).await;
        Arc::new(Self {
            workspaces: workspaces.clone(),
            workspace: reply.workspace.clone(),
            workspace_id,
            agent: turn.agent.clone(),
            offered,
            service: reply.provider.clone(),
        })
    }

    /// Take the telling `request` makes as `caller`: its person is asked, or the server's
    /// refusal.
    async fn take(
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
        let workspaces = &self.workspaces;
        let taken = workspaces
            .connect_calendar_for_turn(&hosted, request, asked_in(caller))
            .await?;
        let words = match taken {
            TurnConnect::Waits => WAITS.to_owned(),
            TurnConnect::Asks(open) => {
                match put(&self.service, caller, &self.workspace, open.as_ref()).await {
                    Ok(()) => ASKED.to_owned(),
                    Err(why) => {
                        workspaces.connect_not_asked(&open);
                        format!(
                            "Your person could not be asked for the password ({why}), so nothing was \
                         connected."
                        )
                    }
                }
            }
        };
        Ok(InvocationUpdate::Failed {
            invocation: request.invocation,
            error: ActionFailure {
                code: ASKED_FIRST.to_owned(),
                message: words,
                details: None,
                retryable: false,
            },
        })
    }
}

impl TurnProvider for TurnConnects {
    /// The provider of a person's accounts, as a listing of everything names it beside the
    /// workspace's resources, where this turn is offered the connecting.
    fn providers(&self, _caller: &Caller) -> Vec<CatalogueProvider> {
        if self.offered.is_empty() {
            return Vec::new();
        }
        vec![CatalogueProvider {
            provider: person_accounts::PROVIDER.to_owned(),
            app_id: person_accounts::PROVIDER.to_owned(),
            workspace: self.workspace.clone(),
            document_id: String::new(),
            browsable: true,
        }]
    }

    /// The connecting's row, when `query` asks the provider of a person's accounts.
    fn rows(&self, _caller: &Caller, query: &CatalogueQuery) -> Option<Rows> {
        (query.provider.as_deref() == Some(person_accounts::PROVIDER) && !self.offered.is_empty())
            .then(|| Pending::ready(Ok(self.offered.clone())))
    }

    /// The connecting of a calendar is the server's entry's to answer, also for a turn it is
    /// not offered to, which the entry then refuses.
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        target.provider == person_accounts::PROVIDER
            && target.address.resource == CALENDARS
            && target.address.action == CALENDAR_CONNECT
    }

    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move { self.take(&caller, &request).await })
    }
}

/// How the question about a calendar to connect stands, where it is not over.
pub(super) enum Stands {
    /// Its person has not answered.
    Waiting,
    /// Its person answered: the reference of what they typed, none where they declined.
    Answered(Option<String>),
    /// This server said it started on the answer.
    Started,
}

/// The question this server put about an ask, where the workspace's chat holds it.
pub(super) struct Found {
    /// The part of the chat's messages that holds the question.
    pub(super) document: String,
    pub(super) message: String,
    /// The conversation it stands in.
    pub(super) conversation_id: String,
    /// Whether it stands as this server asked it: it alone carries the ask, it says the
    /// server's words, and it was never edited.
    pub(super) as_asked: bool,
    pub(super) stands: Stands,
}

/// The reference an answer's `detail` carries, as a person's device wrote it.
fn reference_of(detail: &str) -> Option<String> {
    serde_json::from_str::<Value>(detail)
        .ok()?
        .get("reference")?
        .as_str()
        .map(str::to_owned)
}

/// The question this server put about `ask`, where `parts` hold it and it is not over: the one
/// its agent wrote for that person that asks for a login at the calendar server under the user
/// name. A question that ended, by whoever, is not it.
pub(super) fn question_about(
    parts: &[(String, chat::parts::ChatMessages)],
    ask: &dyn PutAsk,
) -> Option<Found> {
    let (agent, person) = (
        MemberId::agent(ask.by_agent().to_owned()),
        MemberId::person(ask.of_person().to_owned()),
    );
    let shown = ask.shown();
    let mut open = parts
        .iter()
        .flat_map(|(document, part)| {
            part.messages
                .iter()
                .map(move |(id, message)| (document, id, message))
        })
        .filter_map(|(document, id, message)| {
            let question = message.question.as_ref()?;
            let asks = message.author == agent
                && question.kind.0 == chat::question::SECRET
                && question.person == person
                && question.kept().as_ref() == Some(&shown);
            if !asks {
                return None;
            }
            let stands = match question.state() {
                Waiting => Stands::Waiting,
                Answered(given) => Stands::Answered(
                    (given.answer.0 == chat::question::FILLED)
                        .then(|| reference_of(&given.detail.0))
                        .flatten(),
                ),
                Settled(_, outcome) if outcome.outcome.0 == chat::question::STARTED => {
                    Stands::Started
                }
                Settled(..) | Withdrawn(_) => return None,
            };
            Some((document, id, message, stands))
        });
    let (document, id, message, stands) = open.next()?;
    // An agent may edit a message it wrote and may ask questions of its own: neither is what
    // its person was asked.
    let alone = open.next().is_none();
    let as_asked = alone
        && message.text.trim() == ask.said().trim()
        && !message.edited
        && message.history.is_empty();
    Some(Found {
        document: document.clone(),
        message: id.to_string(),
        conversation_id: message.channel.to_string(),
        as_asked,
        stands,
    })
}

/// The calendars of `connected` by their names, as many as fit: `none` where the server lists
/// none.
fn names_of(connected: &Connected, none: &str) -> String {
    let mut names = String::new();
    let mut left = connected.calendars.len();
    for name in &connected.calendars {
        if names.len().saturating_add(name.len()) > MOST_NAMES_BYTES {
            break;
        }
        if !names.is_empty() {
            names.push_str(", ");
        }
        names.push('"');
        names.push_str(name);
        names.push('"');
        left = left.saturating_sub(1);
    }
    match (names.is_empty(), left) {
        (true, 0) => none.to_owned(),
        (true, _) => format!("{left} calendars"),
        (false, 0) => names,
        (false, _) => format!("{names} and {left} more"),
    }
}

/// An answered question about a calendar to connect, claimed, and who asked it.
struct Carrying {
    workspace: String,
    workspace_id: WorkspaceId,
    agent: Agent,
    claimed: ConnectAsk,
    found: Found,
    /// The reference of what its person typed; none where they declined.
    typed: Option<String>,
    /// The agent that asked, acting for its person now: as the turn that asked, and outside
    /// any turn.
    turn: Caller,
    outside: Caller,
}

impl Carrier {
    /// Carry out what the people of `workspace` answered where they were asked for the password
    /// of a calendar to connect: each answer once, claimed and said on its question before
    /// this returns, so what the look reads next finds no answer of these left to wake anybody
    /// for. Signing in to a calendar server goes on a task of its own. Costs one look at what
    /// this server keeps when nothing waits.
    pub(super) async fn settle_connects(&self, workspace: &str) {
        let open = self.workspaces.open_connects(workspace);
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
            let Some(found) = question_about(&parts, &ask) else {
                // Its question is not in the conversation, or is over: one this server
                // stopped on, or one that was never asked, connects nothing any more.
                let old = Duration::from_millis(
                    tilo_clock::unix_now_ms().saturating_sub(ask.asked_at_ms),
                ) > UNASKED_FOR;
                if ask.interrupted || old {
                    self.workspaces.connect_not_asked(&ask);
                }
                continue;
            };
            let asker = match agents.get(&ask.agent) {
                Some(agent) => self
                    .connect_asker(workspace, workspace_id, &ask, agent)
                    .await
                    .map(|callers| (agent.clone(), callers)),
                None => None,
            };
            let Some((agent, (turn, outside))) = asker else {
                // The agent no longer works here, or its person is admitted here on no
                // device: nobody acts on the answer.
                self.workspaces.connect_not_asked(&ask);
                continue;
            };
            let service = WorkspaceActionProvider::new(self.actions.clone(), workspace_id);
            let at = (found.document.as_str(), found.message.as_str());
            if ask.interrupted {
                // This server stopped while it connected the calendar: it is not connected a
                // second time, and its person is told.
                self.workspaces.connect_not_asked(&ask);
                let told = Some(INTERRUPTED.to_owned());
                let failed = chat::question::FAILED;
                if let Err(error) = settle((&service, &outside), workspace, at, failed, told).await
                {
                    tracing::warn!(%workspace, %error, "an interrupted calendar's question did not take how it ended");
                }
                continue;
            }
            let typed = match &found.stands {
                Stands::Waiting => continue,
                Stands::Answered(typed) => typed.clone(),
                // Said to have started, with nothing claimed here: no answer of it is left.
                Stands::Started => {
                    self.workspaces.connect_not_asked(&ask);
                    continue;
                }
            };
            // Claimed before anything is said or done. Claimed by another look already, it is
            // that one's.
            let Ok(claimed) = self.workspaces.claim_connect(&ask.ask) else {
                continue;
            };
            // Only what they typed under the question as it was asked signs in anywhere: that
            // takes a calendar server's time, so it is said to have started here and carried
            // out on a task of its own.
            let signs_in = typed.is_some() && found.as_asked;
            if signs_in
                && let Err(error) = settle(
                    (&service, &outside),
                    workspace,
                    at,
                    chat::question::STARTED,
                    None,
                )
                .await
            {
                // It cannot be said to have started (the question was taken back meanwhile):
                // nothing is connected, and the ask is spent.
                tracing::warn!(%workspace, %error, "a calendar's question did not take that it started");
                self.workspaces.connect_not_asked(&claimed);
                continue;
            }
            let carrying = Carrying {
                workspace: workspace.to_owned(),
                workspace_id,
                agent,
                claimed,
                found,
                typed,
                turn,
                outside,
            };
            if signs_in {
                let this = self.clone();
                tokio::spawn(async move {
                    this.carry_out_connect(carrying).await;
                });
            } else {
                // Declined, or typed under a question that is not the server's: nothing is
                // asked of any calendar server, and how it ended is said before this returns.
                self.carry_out_connect(carrying).await;
            }
        }
    }

    /// The agent that asked `ask`, acting for its person now: as the turn that asked, which is
    /// what the entry decides it by, and outside any turn, which is how it says what came of
    /// the answer once that turn has ended. `None` when the workspace admits the person on no
    /// device.
    pub(super) async fn connect_asker(
        &self,
        workspace: &str,
        workspace_id: WorkspaceId,
        ask: &dyn PutAsk,
        agent: &Agent,
    ) -> Option<(Caller, Caller)> {
        let mut person = [0; 32];
        hex::decode_to_slice(ask.of_person(), &mut person).ok()?;
        let device = self.workspaces.person_device(workspace_id, person).await?;
        let asked_in: AskedIn = serde_json::from_value(ask.put_in().clone()).ok()?;
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

    /// Carry `carrying` out: let the entry do what it kept for the ask, say on the question how
    /// it ended, and tell the agent that asked, once.
    async fn carry_out_connect(&self, carrying: Carrying) {
        let Carrying {
            workspace,
            workspace_id,
            agent,
            claimed,
            found,
            typed,
            turn,
            outside,
        } = carrying;
        let narrowed = narrowing(&agent, &workspace);
        let hosted = HostedTurn {
            workspace: workspace_id,
            caller: &turn,
            keeper: &agent.keeper,
            bound: narrowed.as_deref(),
        };
        let carried = self
            .workspaces
            .carry_out_connect(&hosted, &claimed, typed.as_deref(), found.as_asked)
            .await;
        let (outcome, told, answered, came) = match &carried {
            Ok(Some(connected)) => (
                chat::question::DONE,
                Some(format!(
                    "Connected for reading only. Its calendars: {}.",
                    names_of(connected, "none yet")
                )),
                TYPED,
                format!(
                    "The account is connected for reading only: nothing is ever written to that \
                     calendar server. You are named for it and read it with the calendar \
                     actions. Its calendars, as the calendar server names them: {}.",
                    names_of(connected, "none yet")
                ),
            ),
            Ok(None) => (
                chat::question::DROPPED,
                None,
                DECLINED,
                "Nothing was connected.".to_owned(),
            ),
            Err(why) => (
                chat::question::FAILED,
                Some(format!(
                    "Your calendar was not connected: {why}. What you typed is not kept."
                )),
                TYPED,
                format!(
                    "It was not connected: {why}. Nothing was kept, and what they typed is not \
                     kept either. Tell them so; they tell you again when they want to retry."
                ),
            ),
        };
        let service = WorkspaceActionProvider::new(self.actions.clone(), workspace_id);
        let at = (found.document.as_str(), found.message.as_str());
        if let Err(error) = settle((&service, &outside), &workspace, at, outcome, told).await {
            tracing::warn!(%workspace, %error, "a calendar's question did not take how it ended");
        }
        let news = super::answers::carried_out(
            (&found.message, &found.conversation_id),
            &claimed.person,
            &claimed.named(),
            (answered, &came),
        );
        if let Err(error) = self.wake(&workspace, &news).await {
            tracing::warn!(%workspace, %error, "an agent was not told how connecting a calendar ended");
        }
    }
}
