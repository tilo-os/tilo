//! What a turn on this server reaches of its person's mail.
//!
//! An agent's tools act on the workspace's action service, which knows the workspace's
//! resources and nothing of a person's own mail. That mail is the server's own to answer,
//! through its entry for a turn it carries out
//! ([`crate::workspaces::Workspaces::mail_for_turn`]): the turn is one this server carries out, and its
//! person named the agent for the account. [`TurnMail`] is that entry as one turn's tools reach
//! it, a provider of the turn's own ([`TurnProvider`]): it lists the mail actions the turn is
//! offered beside the workspace's resources, and takes them. So an assistant lists what
//! arrived, reads a message with the files it carries by name and size, searches and keeps a
//! draft, with every device of its person off.
//!
//! A send asks, every time. The entry sets the message apart and answers what its person is to
//! be shown, and this puts the question to them (`ask` of this server's agents): into the
//! person's own conversation with the agent, whatever conversation the turn works in, because
//! the question shows whom a message of theirs goes to and what it says. The tool call ends at
//! once, told that its person is asked, and nothing waits on a thread. The question offers
//! allow and deny, and no answer of always. When the person answers, on any device of theirs,
//! the server carries out what it kept ([`Sends`]): the message leaves, once, and only while
//! the question still says exactly what the server asked.

use std::sync::Arc;

use serde_json::json;
use tilo_actions::policy::MemberId;
use tilo_actions::remote::person_asking::Words;
use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeRequest,
    Pending, RemoteActionTarget, person_accounts,
};
use tilo_actions::{ActionFailure, ActionOutput, InvocationUpdate};
use tilo_workspace_agent::Agent;

use super::asks::{Answered, Carried, Carries, ServerAsk};
use super::turn_provider::{Ending, Rows};
use super::{CHAT, Reply, ServerAgents as Agents, Turn, TurnProvider, WorkspaceId};
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::{
    AskedSend, HostedTurn, MAIL_CONNECT, SEND_CARRIER, TurnMail as Taken,
};
use crate::workspaces::person_asking::kept_asks::KeptAsk;

/// What an agent is told when its person is asked before a message leaves: nothing was sent,
/// and it neither waits nor sends again.
const ASKED: &str = "Your person is asked in your conversation with them whether this message \
     may leave. Nothing was sent. It leaves when they allow it there: tell them it waits for \
     their answer, and do not send it again.";

/// The failure code of a send that was not made because its person is asked first. It is not
/// the code of an action that wants a confirmation: the agent's tools word that one
/// themselves, and the words here are the ones the agent must read.
const ASKED_FIRST: &str = "asked_person_first";

/// What an answer is told when the question it was given to no longer says what this server
/// asked.
const CHANGED: &str = "The question no longer says what this server asked, so nothing was sent.";

/// What one turn reaches of its person's mail. A turn's caller changes when its person writes
/// again while it works, so each request is taken as the caller of the tool call that made it.
pub(super) struct TurnMail {
    agents: Agents,
    workspace: String,
    /// The workspace's id; `None` where the turn names none, which reaches nothing.
    workspace_id: Option<WorkspaceId>,
    agent: Agent,
    /// The mail actions this turn is offered: none for a turn the entry refuses.
    offered: Vec<CatalogueRow>,
}

impl TurnMail {
    /// What `turn` reaches of its person's mail, where `reply` holds what its tools begin as.
    pub(super) async fn for_turn(agents: &Agents, turn: &Turn, reply: &Reply) -> Arc<Self> {
        let workspace_id = WorkspaceId::parse(&reply.workspace);
        let narrowed = narrowing(&turn.agent, &reply.workspace);
        let offered = if let Some(workspace) = workspace_id {
            let hosted = HostedTurn {
                workspace,
                caller: &reply.caller,
                bound: narrowed.as_deref(),
            };
            agents.workspaces.mail_offered(&hosted).await
        } else {
            Vec::new()
        };
        Arc::new(Self {
            agents: agents.clone(),
            workspace: reply.workspace.clone(),
            workspace_id,
            agent: turn.agent.clone(),
            offered,
        })
    }

    /// Take the mail request `request` as `caller`: how it ended, or the server's refusal.
    async fn take(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let invocation = request.invocation;
        let Some(workspace) = self.workspace_id else {
            return Err(ActionCallError::UnknownAction);
        };
        let narrowed = narrowing(&self.agent, &self.workspace);
        let hosted = HostedTurn {
            workspace,
            caller,
            bound: narrowed.as_deref(),
        };
        let workspaces = &self.agents.workspaces;
        let asked = match workspaces.mail_for_turn(&hosted, request).await? {
            Taken::Done(value) => {
                return Ok(InvocationUpdate::Succeeded {
                    invocation,
                    output: ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                });
            }
            Taken::Asks(asked) => asked,
        };
        let words = match self.put(&asked).await {
            Ok(()) => ASKED.to_owned(),
            Err(why) => {
                workspaces.send_not_asked(&asked);
                format!(
                    "Your person could not be asked whether this message may leave ({why}), so \
                     nothing was sent. It is still begun."
                )
            }
        };
        Ok(InvocationUpdate::Failed {
            invocation,
            error: ActionFailure {
                code: ASKED_FIRST.to_owned(),
                message: words,
                details: None,
                retryable: false,
            },
        })
    }

    /// Put the question about `asked` to its person, in their own conversation with the agent:
    /// what it shows of their mail is theirs alone to read.
    async fn put(&self, asked: &AskedSend) -> Result<(), String> {
        let mut person = [0; 32];
        hex::decode_to_slice(&asked.person, &mut person)
            .map_err(|_| "the turn names no person".to_owned())?;
        let kept = serde_json::to_value(asked).map_err(|error| error.to_string())?;
        self.agents
            .ask(ServerAsk {
                workspace: self.workspace.clone(),
                person,
                agent: self.agent.id.0.clone(),
                conversation: String::new(),
                words: asked.words(),
                carrier: SEND_CARRIER.to_owned(),
                kept,
                // Whether anything may let mail leave unasked is its person's to decide, and
                // no answer here does.
                always: None,
                named: Words {
                    action: "Send a message".to_owned(),
                    on: asked.account.clone(),
                    place: String::new(),
                },
            })
            .await
            .map(drop)
    }
}

impl TurnProvider for TurnMail {
    /// The provider of a person's accounts, as a listing of everything names it beside the
    /// workspace's resources, where this turn is offered anything of mail.
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

    /// The mail rows, when `query` asks the provider of a person's accounts.
    fn rows(&self, _caller: &Caller, query: &CatalogueQuery) -> Option<Rows> {
        (query.provider.as_deref() == Some(person_accounts::PROVIDER) && !self.offered.is_empty())
            .then(|| Pending::ready(Ok(self.offered.clone())))
    }

    /// Whatever is asked of a person's mail accounts is the server's entry's to answer, also
    /// where it refuses it. A use of their vault by reference is another provider's, asked
    /// before this one, and signing an account in from its address alone is another's too
    /// ([`super::mail_connect`]).
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        target.provider == person_accounts::PROVIDER
            && target.address.resource == person_accounts::RESOURCE
            && target.address.action != MAIL_CONNECT
    }

    /// What a message holds was written by whoever sent it: its words, what it is about, the
    /// name its sender goes by and the names of the files with it. Whatever answers messages
    /// (what arrived, a folder, one message, what a search found) carries them, and the turn's
    /// tools are told so with the answer. What this server says itself, of a message it began
    /// or of a question it put to its person, is no content from outside.
    fn carries_untrusted(&self, target: &RemoteActionTarget) -> bool {
        matches!(
            target.address.action.as_str(),
            person_accounts::ARRIVED
                | person_accounts::FETCH
                | person_accounts::READ
                | person_accounts::SEARCH
        )
    }

    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move { self.take(&caller, &request).await })
    }
}

/// The rules a turn of `agent` is decided by in `workspace`, where anything narrows it: what
/// its entry bounds it to and, for a sub-agent, what keeps it from whoever started it. `None`
/// for an agent nothing narrows.
pub(super) fn narrowing(agent: &Agent, workspace: &str) -> Option<Vec<tilo_action_policy::Rule>> {
    (agent.bound_rules().is_some() || agent.is_sub()).then(|| agent.turn_rules(workspace))
}

/// What carries out a person's answer to the question about a message their agent would send.
struct Sends {
    workspaces: Workspaces,
}

impl Sends {
    /// Whether the question about `ask` still says `words`, exactly as this server asked: an
    /// agent may edit a message it wrote, and a question that was ever edited, or that stands
    /// beside another under the same ask, is not the one its person was asked.
    async fn says(&self, ask: &KeptAsk, words: &str) -> bool {
        let agent = MemberId::agent(ask.agent.clone());
        let person = MemberId::person(ask.person.clone());
        let shown = json!({"ask": ask.ask});
        let routes = self
            .workspaces
            .document_routes(&ask.workspace)
            .await
            .unwrap_or_default();
        let mut found = false;
        for (app, document) in routes {
            if app != CHAT
                || collab_types::document::DocumentId::parse(&document)
                    .is_none_or(|part| chat::parts::part_index(&part).is_none())
            {
                continue;
            }
            let Ok(Some(saved)) = self
                .workspaces
                .resource_document(&ask.workspace, CHAT, &document)
                .await
            else {
                continue;
            };
            let Ok(mut held) = automerge::AutoCommit::load(&saved) else {
                continue;
            };
            let Ok(part) = crate::workspace_bots::opened::<chat::parts::ChatMessages>(&mut held)
            else {
                continue;
            };
            for (_, message) in part.messages.iter() {
                let asks = message.author == agent
                    && message.question.as_ref().is_some_and(|question| {
                        question.person == person && question.kept().as_ref() == Some(&shown)
                    });
                if !asks {
                    continue;
                }
                let as_asked = message.text == words
                    && !message.edited
                    && message.history.is_empty()
                    && message.body.is_empty();
                if !as_asked {
                    return false;
                }
                found = true;
            }
        }
        found
    }
}

impl Carries for Sends {
    /// Only its person's allow lets the message leave, and only while the question says what
    /// the server asked. Whatever they answered, what waited is gone after it.
    fn carry<'a>(&'a self, ask: &'a KeptAsk, answer: Answered) -> Carried<'a> {
        Box::pin(async move {
            let mut changed = false;
            if answer == Answered::Once {
                let words = serde_json::from_value::<AskedSend>(ask.kept.clone())
                    .map(|asked| asked.words())
                    .unwrap_or_default();
                changed = !self.says(ask, words.trim()).await;
            }
            let allowed = answer == Answered::Once && !changed;
            let left = self.workspaces.send_asked(ask, allowed).await.map(|_| None);
            if changed {
                return Err(CHANGED.to_owned());
            }
            left
        })
    }
}

impl Agents {
    /// Carry out the answers to the questions about messages from now on: named when this
    /// server's agents start, because a person who answers after a restart starts no turn.
    pub(super) fn carry_sends(&self) {
        let sends = Sends {
            workspaces: self.workspaces.clone(),
        };
        self.carries(SEND_CARRIER, Arc::new(sends));
    }
}
