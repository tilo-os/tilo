//! What a turn on this host uses of its person's vault, by reference.
//!
//! An agent's tools run on the workspace's action service, which knows the
//! workspace's records and nothing of a person's own vault. The uses of a
//! vault item by reference (adding a mail account whose password the vault
//! keeps, keeping a provider's key from it) are the host's own to answer,
//! through its entry for a turn it runs
//! ([`crate::workspaces::hosted_use`]). [`TurnUses`] is that entry as one
//! turn's tools reach it: it lists the uses the turn is offered beside the
//! workspace's records, and takes them.
//!
//! No device of the person's is on, so the first use of an item somewhere
//! asks them. The question is Chat's own, of the kind `action`, put in the
//! conversation the agent works in as the turn's caller, and the tool call
//! ends at once: nothing waits on a thread. When the person answers, on any
//! device of theirs, the host reads the answer from the workspace's record
//! ([`HostAgents::settle_uses`]) and carries out what it kept for that
//! question itself: `allow` once, `always` with a standing leave written
//! first, `deny` not at all. It says `started` before it runs anything, so
//! an answer is carried out at most once, and then how it ended.
//!
//! Nothing here sees a secret. The arguments name the item by its
//! reference, the question shows it withheld, and the entry answers what
//! the person would be answered: the account, the provider.

use std::collections::BTreeMap;
use std::sync::Arc;

use chat::question::Standing;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tilo_actions::policy::MemberId;
use tilo_actions::remote::{
    ActionHostError, Caller, CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueRow,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, ReceiptStatus, RemoteActionTarget,
    person_vault,
};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_protocol::WorkspaceId;
use tilo_resource::Resource as _;
use tilo_workspace_agent::Agent;

use super::{Ask, CHAT, HostAgents, Turn, WorkspaceActionHost, invoke_named};
use crate::workspaces::hosted_use::{HostedTurn, OpenAsk, TurnUse, UseAnswer};

/// What an agent is told when its person is asked first: nothing ran, and
/// it neither waits nor asks again.
const ASKED: &str = "Your person is asked in the conversation whether you may use this vault item \
     here. Nothing was used yet. It runs when they allow it there: tell them it waits for their \
     answer, and do not make this call again.";

/// The failure code of a use that did not run because its person is asked
/// first. It is not the code of an action that wants a confirmation: the
/// agent's tools word that one themselves, and the words here are the ones
/// the agent must read.
const ASKED_FIRST: &str = "asked_person_first";

/// What the question says when the agent gave no reason of its own.
const NO_REASON: &str = "May I use this item from your vault here?";

/// Where a turn's question is put, and for which turn: kept with the ask,
/// so the host finds its way back after a restart.
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
    agents: HostAgents,
    runtime: tokio::runtime::Handle,
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
    host: WorkspaceActionHost,
    /// How each use this turn took ended, by its invocation.
    ended: std::sync::Mutex<Vec<(InvocationId, InvocationState)>>,
}

impl TurnUses {
    /// What `turn`, whose tools begin as `caller`, uses of its person's
    /// vault in `workspace`.
    pub(super) async fn for_turn(
        agents: &HostAgents,
        (workspace, workspace_id): (&str, WorkspaceId),
        turn: &Turn,
        caller: &Caller,
        host: WorkspaceActionHost,
    ) -> Arc<Self> {
        let bound = turn.agent.bound_rules();
        let hosted = HostedTurn {
            workspace: workspace_id,
            caller,
            keeper: &turn.agent.keeper,
            bound: bound.as_deref(),
        };
        let offered = agents.workspaces.uses_offered(&hosted).await;
        Arc::new(Self {
            agents: agents.clone(),
            runtime: tokio::runtime::Handle::current(),
            workspace: workspace.to_owned(),
            workspace_id,
            agent: turn.agent.clone(),
            person: turn.person.clone(),
            ask: turn.ask.clone(),
            offered,
            host,
            ended: std::sync::Mutex::default(),
        })
    }

    /// Whether `target` is a use by reference: this host's entry answers
    /// it, also for a turn it offers none, which the entry then refuses.
    pub(super) fn takes(target: &RemoteActionTarget) -> bool {
        person_vault::use_of(target).is_some()
    }

    /// The providers of the uses this turn is offered, as a listing of
    /// everything names them beside the workspace's records.
    pub(super) fn providers(&self) -> Vec<CatalogueProvider> {
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
    pub(super) fn page(&self, query: &CatalogueQuery) -> Option<CataloguePage> {
        let provider = query.provider.as_deref()?;
        let rows: Vec<CatalogueRow> = self
            .offered
            .iter()
            .filter(|row| row.provider == provider)
            .cloned()
            .collect();
        (!rows.is_empty()).then(|| CataloguePage::from_rows("0".into(), rows, query))
    }

    /// Take the use `request` asks for as `caller`, answered once it ran or
    /// its person is asked. How it ended is kept with the turn before the
    /// caller is answered, because the agent's tools read an invocation's
    /// end from the host that took it ([`Self::observed`]), not from its
    /// receipt.
    pub(super) fn invoke(
        self: &Arc<Self>,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let (completer, answer) = tilo_actions::remote::pending();
        let (uses, caller) = (Arc::clone(self), caller.clone());
        self.runtime.spawn(async move {
            let invocation = request.invocation;
            let receipt = match uses.take(&caller, &request).await {
                Ok(update) => {
                    let state = InvocationState::new(invocation, update);
                    uses.ended.lock_recover().push((invocation, state.clone()));
                    state.receipt()
                }
                Err(error) => InvocationReceipt {
                    invocation,
                    status: ReceiptStatus::Rejected { error },
                },
            };
            completer.complete(Ok(receipt));
        });
        answer
    }

    /// How the use `id` names ended, when this turn took it.
    pub(super) fn observed(&self, id: &InvocationId) -> Option<InvocationState> {
        self.ended
            .lock_recover()
            .iter()
            .find(|(taken, _)| taken == id)
            .map(|(_, state)| state.clone())
    }

    /// Take the use `request` asks for as `caller`: how it ended, or the
    /// host's refusal.
    async fn take(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, ActionHostError> {
        let invocation = request.invocation;
        let bound = self.agent.bound_rules();
        let hosted = HostedTurn {
            workspace: self.workspace_id,
            caller,
            keeper: &self.agent.keeper,
            bound: bound.as_deref(),
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
            TurnUse::Waits => ASKED.to_owned(),
            TurnUse::Asks(open) => match self.put(caller, &open).await {
                Ok(()) => ASKED.to_owned(),
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

    /// Put the question about `open` to the person, in the conversation
    /// the turn works in, as the turn's caller.
    async fn put(&self, caller: &Caller, open: &OpenAsk) -> Result<(), String> {
        let channel = self.channel().await?;
        let text = if open.reason.is_empty() {
            NO_REASON
        } else {
            open.reason.as_str()
        };
        let mut arguments = Map::new();
        arguments.insert("channel".into(), json!(channel));
        arguments.insert("text".into(), json!(text));
        arguments.insert("kind".into(), json!(chat::question::ACTION));
        arguments.insert(
            "answers".into(),
            json!([
                {"id": chat::question::ALLOW},
                {"id": chat::question::ALWAYS},
                {"id": chat::question::DENY},
            ]),
        );
        arguments.insert("detail".into(), open.detail.clone());
        let route = DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: self.workspace.clone(),
            document: String::new(),
        };
        invoke_named(
            &self.host,
            caller,
            &route,
            &ui::resource::chain_resource_id(&chat::parts::chain()),
            "ask",
            arguments,
        )
        .await
        .map(drop)
    }

    /// The conversation the question goes to: the one the turn's ask was
    /// written in, or for an issue the person's own conversation with the
    /// agent.
    async fn channel(&self) -> Result<String, String> {
        match &self.ask {
            Ask::Message { part, message, .. } => {
                let held = self
                    .agents
                    .read::<chat::parts::ChatPart>(&self.workspace, CHAT, part)
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

/// The failure of a use that did not run because its person is asked:
/// `words` tell the agent so.
fn asked(words: String) -> ActionFailure {
    ActionFailure {
        code: ASKED_FIRST.to_owned(),
        message: words,
        details: None,
        retryable: false,
    }
}

/// The question this host put about `ask`, where the workspace's chat holds
/// it: the part's record, the message and the question. A question that
/// carries the ask's id and shows anything else than this host asked is not
/// it, and neither is one its agent did not write for that person.
fn question_about<'a>(
    parts: &'a [(String, chat::parts::ChatPart)],
    ask: &OpenAsk,
) -> Option<(&'a str, String, &'a chat::question::Question)> {
    let (agent, person) = (
        MemberId::agent(ask.agent.clone()),
        MemberId::person(ask.person.clone()),
    );
    parts.iter().find_map(|(record, part)| {
        part.messages.iter().find_map(|(id, message)| {
            let question = message.question.as_ref()?;
            (question.kind.0 == chat::question::ACTION
                && message.author == agent
                && question.person == person
                && question.kept().as_ref() == Some(&ask.detail))
            .then(|| (record.as_str(), id.to_string(), question))
        })
    })
}

/// Say on the question `message` in the part `record` what came of its
/// answer.
async fn settle(
    (host, asker): (&WorkspaceActionHost, &Caller),
    workspace: &str,
    (record, message): (&str, &str),
    outcome: &str,
    told: Option<String>,
) -> Result<(), String> {
    let route = DocumentRoute {
        app_id: CHAT.to_owned(),
        cluster_id: workspace.to_owned(),
        document: record.to_owned(),
    };
    let mut arguments = Map::new();
    arguments.insert("message".into(), json!(message));
    arguments.insert("outcome".into(), json!(outcome));
    if let Some(told) = told {
        arguments.insert("detail".into(), json!(told));
    }
    let resource = format!("{}/messages/{message}", chat::parts::ChatPart::KIND);
    invoke_named(host, asker, &route, &resource, "settle-question", arguments)
        .await
        .map(drop)
}

/// An answered question about a use, and who asked it.
struct Answered {
    ask: OpenAsk,
    /// The part of the chat's messages that holds the question.
    record: String,
    message: String,
    answer: UseAnswer,
    /// The question's detail as the workspace's record holds it: what its
    /// person was shown when they answered.
    shown: Value,
}

impl HostAgents {
    /// Carry out what the people of `workspace` answered about the uses its
    /// agents asked for: each answer once, started before it runs. Costs one
    /// look at what this host keeps when nothing waits.
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
            let Some((record, message, question)) = question_about(&parts, &ask) else {
                // Its question never reached the record: nothing to answer.
                if ask.interrupted {
                    self.workspaces.use_interrupted(&ask);
                }
                continue;
            };
            let shown = question.kept().unwrap_or(Value::Null);
            let answered = |answer| Answered {
                ask: ask.clone(),
                record: record.to_owned(),
                message: message.clone(),
                answer,
                shown: shown.clone(),
            };
            let answer = match question.standing() {
                Standing::Waiting if !ask.interrupted => continue,
                Standing::Answered(given) if !ask.interrupted => match given.answer.0.as_str() {
                    chat::question::ALLOW => UseAnswer::Once,
                    chat::question::ALWAYS => UseAnswer::Always,
                    _ => UseAnswer::No,
                },
                // Taken back, or said to have ended by somebody else: it
                // runs nothing any more.
                Standing::Withdrawn(_) | Standing::Settled(..) if !ask.interrupted => {
                    self.workspaces.use_not_asked(&ask);
                    continue;
                }
                // This host stopped while it carried the answer out: it is
                // not carried out a second time, and its person is told.
                _ => {
                    self.interrupted(workspace_id, &agents, answered(UseAnswer::No))
                        .await;
                    continue;
                }
            };
            let Some(agent) = agents.get(&ask.agent) else {
                // The agent no longer works here: nobody acts on the answer.
                self.workspaces.use_not_asked(&ask);
                continue;
            };
            let (this, agent, carrying) = (self.clone(), agent.clone(), answered(answer));
            let workspace = workspace.to_owned();
            tokio::spawn(async move {
                this.carry_out_use((&workspace, workspace_id), &agent, carrying)
                    .await;
            });
        }
    }

    /// Every part of the chat's messages `workspace` holds, by its record.
    async fn chat_parts(&self, workspace: &str) -> Vec<(String, chat::parts::ChatPart)> {
        let routes = self
            .workspaces
            .document_routes(workspace)
            .await
            .unwrap_or_default();
        let mut parts = Vec::new();
        for (app, record) in routes {
            if app != CHAT
                || ui::resource::DocumentId::parse(&record)
                    .is_none_or(|part| chat::parts::part_index(&part).is_none())
            {
                continue;
            }
            if let Ok(Some(part)) = self
                .read::<chat::parts::ChatPart>(workspace, CHAT, &record)
                .await
            {
                parts.push((record, part));
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

    /// Carry out `answered`: say it started, let the entry run what it kept
    /// for the ask, and say how it ended.
    async fn carry_out_use(
        &self,
        (workspace, workspace_id): (&str, WorkspaceId),
        agent: &Agent,
        answered: Answered,
    ) {
        let Answered {
            ask,
            record,
            message,
            answer,
            shown,
        } = answered;
        let Some((turn, outside)) = self.asker(workspace, workspace_id, &ask, agent).await else {
            // Its person is admitted here on no device any more.
            self.workspaces.use_not_asked(&ask);
            return;
        };
        let host = WorkspaceActionHost::new(self.actions.clone(), workspace_id);
        let said = (&host, &outside);
        let at = (record.as_str(), message.as_str());
        // Claimed before anything is said or run: saying that it started
        // wakes the next look, which finds the ask taken and leaves it to
        // this one. Claimed by another look already, it is that one's.
        let Ok(claimed) = self.workspaces.claim_use(&ask.ask) else {
            return;
        };
        if answer != UseAnswer::No
            && let Err(error) = settle(said, workspace, at, chat::question::STARTED, None).await
        {
            // It cannot be said to have started (the question was taken back
            // meanwhile): nothing runs, and the ask is spent.
            tracing::warn!(%workspace, %error, "a use's question did not take that it started");
            self.workspaces.use_not_asked(&claimed);
            return;
        }
        let bound = agent.bound_rules();
        let hosted = HostedTurn {
            workspace: workspace_id,
            caller: &turn,
            keeper: &agent.keeper,
            bound: bound.as_deref(),
        };
        let carried = self
            .workspaces
            .carry_out_use(&hosted, &claimed, &shown, answer)
            .await;
        let (outcome, told) = match carried {
            Ok(Some(_)) => (chat::question::DONE, None),
            Ok(None) => (chat::question::DROPPED, None),
            Err(error) => (chat::question::FAILED, Some(error.to_string())),
        };
        if let Err(error) = settle(said, workspace, at, outcome, told).await {
            tracing::warn!(%workspace, %error, "a use's question did not take how it ended");
        }
    }

    /// The host stopped while it carried out the answer `answered` names:
    /// say that it could not be done, and keep it from running again.
    async fn interrupted(
        &self,
        workspace_id: WorkspaceId,
        agents: &BTreeMap<String, Agent>,
        answered: Answered,
    ) {
        let Answered {
            ask,
            record,
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
        let host = WorkspaceActionHost::new(self.actions.clone(), workspace_id);
        let told =
            "The server restarted while this was being done. Ask again if it is still needed.";
        if let Err(error) = settle(
            (&host, &outside),
            &ask.workspace,
            (record.as_str(), message.as_str()),
            chat::question::FAILED,
            Some(told.to_owned()),
        )
        .await
        {
            tracing::warn!(workspace = %ask.workspace, %error, "an interrupted use's question did not take how it ended");
        }
    }
}
