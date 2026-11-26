//! What a turn on this server reaches of the services its person connected.
//!
//! An agent's tools act on the workspace's action service, which knows the workspace's
//! resources and nothing of a person's own connections. Those are the server's own to answer,
//! through its entry for a turn it carries out
//! ([`crate::workspaces::Workspaces::connections_for_turn`]): the turn is one this server carries out, and
//! an agent that anything narrows reaches only a connection a rule of its own names.
//! [`TurnConnections`] is that entry as one turn's tools reach it, a provider of the turn's own
//! ([`TurnProvider`]): at every listing it names what an agent does with its person's
//! connections and each command of each connection, with whether it asks first, and it takes
//! their calls. So an assistant lists its person's services and calls a command that only
//! reads, with every device of its person off.
//!
//! A command that changes something asks, unless its person let exactly that command of that
//! connection stand for this agent. The entry answers what the person is to be asked, and this
//! puts the question to them (`ask` of this server's agents): into their own conversation with
//! the agent, whatever conversation the turn works in, because it shows what would be sent to
//! a service of theirs. The tool call ends at once, told that its person is asked, and nothing
//! waits on a thread. A connection an agent proposes is asked the same way, and offers no
//! answer of always. When the person answers, on any device of theirs, the server carries out
//! what it kept ([`Answers`]): once, only for the agent that asked acting for that person now,
//! and only while the question still says what the server asked.

use std::future::Future;
use std::sync::Arc;

use serde_json::{Value, json};
use tilo_actions::policy::MemberId;
/// The word the answers about connections are carried out under.
pub(super) use tilo_actions::remote::person_connections::CARRIER as WORD;
use tilo_actions::remote::person_connections::{self, Asks, Leave, Taken};
use tilo_actions::remote::{
    Caller, CatalogueProvider, CatalogueQuery, InvokeRequest, RemoteActionTarget,
};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilo_workspace_agent::Agent;

use super::asks::{Answered, Asker, Carried, Carries, ServerAsk};
use super::turn_provider::{Ending, Rows};
use super::{CHAT, Reply, Turn, TurnProvider, WorkspaceId};
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::HostedTurn;
use crate::workspaces::person_asking::kept_asks::KeptAsk;

/// What an agent is told when its person is asked before a command is called: nothing was
/// called, and it neither waits nor calls again.
const ASKED: &str = "Your person is asked in your conversation with them whether this command \
     may be called. Nothing was called. It is called once when they allow it there: tell them \
     it waits for their answer, and do not call it again.";

/// What an answer is told when the question it was given to no longer says what this server
/// asked.
const CHANGED: &str = "The question no longer says what this server asked, so nothing was done.";

/// What an answer is told when the agent that asked no longer acts for the person asked.
const NOT_THEIRS: &str =
    "The agent that asked no longer acts for the person it asked, so nothing was done.";

/// What one turn reaches of its person's connections. A turn's caller changes when its person
/// writes again while it works, so each request is taken as the caller of the tool call that
/// made it.
/// How a call a turn's provider took ends: what the turn's tools are told, or the server's
/// refusal.
type Ended = <Ending as Future>::Output;

pub(super) struct TurnConnections {
    /// The server's workspaces, and its one way of asking a person.
    asker: Asker,
    workspace: String,
    workspace_id: WorkspaceId,
    agent: Agent,
    /// Whether the turn is one this server carries out: no other turn is told that a person's
    /// connections exist.
    theirs: bool,
    /// Where a listing's rows are read: a tool may list from outside any task.
    runtime: tokio::runtime::Handle,
}

/// What `turn` reaches of its person's connections in the workspace `workspace_id`, where
/// `reply` holds what its tools begin as and `asker` puts a question to its person.
pub(super) async fn of(
    asker: Asker,
    turn: &Turn,
    reply: &Reply,
    workspace_id: WorkspaceId,
) -> Arc<TurnConnections> {
    let narrowed = turn.agent.bound_rules();
    let hosted = HostedTurn {
        workspace: workspace_id,
        caller: &reply.caller,
        bound: narrowed.as_deref(),
    };
    let theirs = asker.workspaces.asked_turn(&hosted).await.is_ok()
        && asker
            .workspaces
            .agent_may_act(
                &reply.workspace,
                &reply.caller,
                person_connections::PROVIDER,
            )
            .await;
    Arc::new(TurnConnections {
        asker,
        workspace: reply.workspace.clone(),
        workspace_id,
        agent: turn.agent.clone(),
        theirs,
        runtime: tokio::runtime::Handle::current(),
    })
}

impl TurnConnections {
    /// Take the request `request` as `caller`: how it ended, or the server's refusal.
    async fn take(&self, caller: &Caller, request: &InvokeRequest) -> Ended {
        let invocation = request.invocation;
        let workspace = self.workspace_id;
        let narrowed = self.agent.bound_rules();
        let hosted = HostedTurn {
            workspace,
            caller,
            bound: narrowed.as_deref(),
        };
        let workspaces = &self.asker.workspaces;
        let asks = match workspaces.connections_for_turn(&hosted, request).await? {
            Taken::Done(value) => return Ok(succeeded(invocation, value)),
            Taken::Asks(asks) => asks,
        };
        let put = self.put(&asks).await;
        Ok(match (put, asks.answers) {
            // Asking was all that was asked for: a proposal.
            (Ok(Some(value)), _) | (Ok(None), Some(value)) => succeeded(invocation, value),
            (Ok(None), None) => asked(invocation, ASKED.to_owned()),
            (Err(why), _) => asked(
                invocation,
                format!("Your person could not be asked ({why}), so nothing was done."),
            ),
        })
    }

    /// Put the question `asks` holds to its person, in their own conversation with the agent:
    /// what it shows of a service of theirs is theirs alone to read.
    async fn put(&self, asks: &Asks) -> Result<Option<Value>, String> {
        self.asker
            .ask(ServerAsk {
                workspace: self.workspace.clone(),
                person: asks.person,
                agent: self.agent.id.0.clone(),
                conversation: String::new(),
                words: asks.words.clone(),
                carrier: person_connections::CARRIER.to_owned(),
                kept: asks.kept.clone(),
                always: asks.always.clone(),
                named: asks.named.clone(),
            })
            .await
    }
}

impl TurnProvider for TurnConnections {
    /// The provider of a person's connections, as a listing of everything names it beside the
    /// workspace's resources, for a turn that is one this server carries out.
    fn providers(&self, _caller: &Caller) -> Vec<CatalogueProvider> {
        if !self.theirs {
            return Vec::new();
        }
        vec![CatalogueProvider {
            provider: person_connections::PROVIDER.to_owned(),
            app_id: person_connections::PROVIDER.to_owned(),
            workspace: self.workspace.clone(),
            document_id: String::new(),
            browsable: true,
        }]
    }

    /// The rows of a person's connections, when `query` asks their provider: read anew at
    /// every listing, so a connection its person added a moment ago is in it.
    fn rows(&self, caller: &Caller, query: &CatalogueQuery) -> Option<Rows> {
        if !self.theirs || query.provider.as_deref() != Some(person_connections::PROVIDER) {
            return None;
        }
        let workspace = self.workspace_id;
        let (completer, rows) = tilo_actions::remote::pending();
        let workspaces = self.asker.workspaces.clone();
        let (caller, agent) = (caller.clone(), self.agent.clone());
        self.runtime.spawn(async move {
            let narrowed = agent.bound_rules();
            let hosted = HostedTurn {
                workspace,
                caller: &caller,
                bound: narrowed.as_deref(),
            };
            completer.complete(Ok(workspaces.connection_rows_for_turn(&hosted).await));
        });
        Some(rows)
    }

    /// Whatever is asked of a person's connections is the server's entry's to answer, also
    /// where it refuses it.
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        target.provider == person_connections::PROVIDER
    }

    /// What a service its person connected says was written by that service: what a command
    /// of it answers, and what it says of itself and of its commands where a turn lists its
    /// person's connections, looks at an address or reads a connection's commands again. The
    /// turn's tools are told so with the answer. Where a call is not made because its person
    /// is asked first, what ends it is this server's own words, and the tools label no call
    /// that was not made.
    fn carries_untrusted(&self, target: &RemoteActionTarget) -> bool {
        matches!(
            target.address.action.as_str(),
            person_connections::CALL
                | person_connections::LIST
                | person_connections::LOOK
                | person_connections::REFRESH
        )
    }

    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move { self.take(&caller, &request).await })
    }
}

fn succeeded(invocation: InvocationId, value: Value) -> InvocationUpdate {
    InvocationUpdate::Succeeded {
        invocation,
        output: ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        },
    }
}

/// The end of a call that was not made because its person is asked first: `words` tell the
/// agent so. It is not the code of an action that wants a confirmation, which the agent's
/// tools word themselves.
fn asked(invocation: InvocationId, words: String) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: ActionFailure {
            code: person_connections::ASKED_FIRST.to_owned(),
            message: words,
            details: None,
            retryable: false,
        },
    }
}

/// What carries out a person's answer to a question about a connection of theirs.
struct Answers {
    workspaces: Workspaces,
    roster: super::Roster,
}

impl Answers {
    /// The agent that asked `ask`, as the workspace's list of its agents holds it now.
    async fn asker(&self, ask: &KeptAsk) -> Option<Agent> {
        let kept = super::roster(&self.workspaces, &ask.workspace)
            .await
            .ok()??;
        kept.get(&tilo_resource::Id::from(ask.agent.as_str()))
            .filter(|agent| agent.is_active())
            .cloned()
    }

    /// Whether the question about `ask` still says `words`, exactly as this server asked: an
    /// agent may edit a message it wrote, and a question that was ever edited, or that stands
    /// beside another under the same ask, is not the one its person was asked.
    async fn says(&self, ask: &KeptAsk, words: &str) -> bool {
        let agent = MemberId::agent(ask.agent.clone());
        let person = MemberId::person(ask.person.clone());
        let named = json!(ask.ask);
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
            let read = (CHAT, document.as_str());
            let Ok(Some(part)) = self
                .workspaces
                .read_resource::<chat::parts::ChatMessages>(&ask.workspace, read)
                .await
            else {
                continue;
            };
            for (_, message) in part.messages.iter() {
                let asks = message.author == agent
                    && message.question.as_ref().is_some_and(|question| {
                        question.person == person
                            && question
                                .kept()
                                .is_some_and(|kept| kept.get("ask") == Some(&named))
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

impl Carries for Answers {
    /// The words this server asked with, kept beside what the question shows.
    fn asked_with(&self, ask: &KeptAsk) -> Option<String> {
        let words = ask.kept.get("words").and_then(Value::as_str);
        words.map(str::to_owned)
    }

    /// Their yes calls the command, or adds the connection, as this server kept it: for the
    /// agent that asked, acting for the person asked as the workspace admits them now, and
    /// only while the question says what the server asked. Their no does nothing.
    fn carry<'a>(&'a self, ask: &'a KeptAsk, answer: Answered) -> Carried<'a> {
        Box::pin(async move {
            let leave = match answer {
                Answered::Once => Some(Leave::Once),
                Answered::Always => Some(Leave::Always),
                Answered::No => None,
            };
            let gone = || NOT_THEIRS.to_owned();
            let workspace = WorkspaceId::parse(&ask.workspace).ok_or_else(gone)?;
            let mut person = [0; 32];
            hex::decode_to_slice(&ask.person, &mut person).map_err(|_| gone())?;
            if self
                .workspaces
                .person_device(workspace, person)
                .await
                .is_none()
            {
                return Err(gone());
            }
            let agent = self.asker(ask).await.ok_or_else(gone)?;
            if leave.is_some() {
                let words = ask.kept.get("words").and_then(Value::as_str);
                if !self.says(ask, words.unwrap_or_default()).await {
                    return Err(CHANGED.to_owned());
                }
            }
            // The agent working for its person now, on a turn of its own: what the entry
            // decides the answer by.
            let caller =
                super::agent_caller(&self.workspaces, &self.roster, workspace, &agent, person)
                    .await?
                    .on_roster_turn(tilo_action_policy::turn::Turn::new(
                        String::new(),
                        ask.ask.clone(),
                    ));
            let narrowed = agent.bound_rules();
            let hosted = HostedTurn {
                workspace,
                caller: &caller,
                bound: narrowed.as_deref(),
            };
            self.workspaces
                .connection_answered(&hosted, ask, leave)
                .await
        })
    }
}

/// What carries out the answers to the questions about connections. It is named when this
/// server's agents start, because a person who answers after a restart starts no turn.
pub(super) fn answers(workspaces: &Workspaces, roster: &super::Roster) -> Arc<dyn Carries> {
    Arc::new(Answers {
        workspaces: workspaces.clone(),
        roster: roster.clone(),
    })
}
