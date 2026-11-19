//! What a turn on this server reaches of its person's mail.
//!
//! An agent's tools act on the workspace's action service, which knows the workspace's
//! resources and nothing of a person's own mail. That mail is the server's own to answer,
//! through its entry for a turn it carries out
//! ([`crate::workspaces::Workspaces::mail_for_turn`]): the turn is its keeper's own, and its
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

use super::asks::{Answered, Carried, Carries, ServerAsk, could_not_ask};
use super::turn_provider::{Ending, Rows};
use super::{CHAT, HostAgents as Agents, Reply, Turn, TurnProvider, WorkspaceId};
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

/// What a turn that reaches its person's mail is told of it: which verbs it has, the call that
/// lists an inbox, and what reaches nothing of mail at all. Mail's verbs act on the person's own
/// accounts, not on an app the workspace keeps, so no path names them that reads or links the
/// mail app instead, and a call spent on one is lost. What came before any of that is the
/// account's id: `fetch`, `read` and `search` never answer without it, and one read of it is
/// worth more than a call they refuse.
pub(super) const TOLD: &str = " Your person's mail is reached through the actions of \
     `accounts/accounts`, which `tilo_ls` with `{\"path\": \"accounts\"}` lists: `list`, \
     `arrived`, `fetch`, `read`, `search`, `begin-send`, `send-piece`, `save-draft` and `send`. \
     Call them with `tilo_do` as `{\"path\": \"accounts/accounts#<verb>\", \"args\": {…}}`, \
     and start from `accounts/accounts#list`: what an account is called and which folders it \
     holds. What already arrived is one `tilo_do` of `accounts/accounts#fetch` with `account` \
     and `folder` (`inbox`), which answers the messages with their senders, their titles and a \
     part of their words; `arrived` answers only what reached the inboxes since a place in the \
     log of what arrived, so it is not how an inbox is read. One message whole is \
     `accounts/accounts#read` with `account`, `folder` and the message's `uid`. A request over \
     one account's folder is one `tilo_do` naming `account` and `folder` once: a second ask \
     goes on from what the first answered. A call that says `did not answer before the timeout` \
     waited out the account's connection, so the same call straight away is very likely to \
     time out again: say that the account did not answer, read what `arrived` already holds \
     without it, and keep anything that would leave as a draft until the account answers. \
     `mail` is no path of these actions: `tilo_read` and `tilo_link` of it answer nothing, so \
     read mail with the verbs and link a message by the path their answer names. Show what an \
     inbox holds with `answer_with_view` once you have it, or say it in words.";

/// What one turn reaches of its person's mail. A turn's caller changes when its person writes
/// again while it works, so each request is taken as the caller of the tool call that made it.
pub(super) struct TurnMail {
    agents: Agents,
    workspace: String,
    /// The workspace's id; `None` where the turn names none, which reaches nothing.
    workspace_id: Option<WorkspaceId>,
    agent: Agent,
    /// Verified active root assistant id when this turn belongs to a sub-agent.
    root_agent: Option<String>,
    /// Explicit narrowing rules from the agent and its starter line (never Chat's one-way rules).
    narrowed: Option<Vec<tilo_action_policy::Rule>>,
    /// The mail actions this turn is offered: none for a turn the entry refuses.
    offered: Vec<CatalogueRow>,
}

impl TurnMail {
    /// What `turn` reaches of its person's mail, where `reply` holds what its tools begin as.
    pub(super) async fn for_turn(agents: &Agents, turn: &Turn, reply: &Reply) -> Arc<Self> {
        let workspace_id = WorkspaceId::parse(&reply.workspace);
        let narrowed = turn_narrowing(turn);
        let root_agent = root_agent_of(turn);
        let offered = if let Some(workspace) = workspace_id {
            let hosted = HostedTurn {
                workspace,
                caller: &reply.caller,
                keeper: &turn.agent.keeper,
                bound: narrowed.as_deref(),
            }
            .under_root(root_agent.as_deref());
            agents.workspaces.mail_offered(&hosted).await
        } else {
            Vec::new()
        };
        Arc::new(Self {
            agents: agents.clone(),
            workspace: reply.workspace.clone(),
            workspace_id,
            agent: turn.agent.clone(),
            root_agent,
            narrowed,
            offered,
        })
    }

    /// Whether this turn is offered anything of its person's mail: what its keeper was named
    /// for an account to reach ([`TOLD`]).
    pub(super) fn offers(&self) -> bool {
        !self.offered.is_empty()
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
        let hosted = HostedTurn {
            workspace,
            caller,
            keeper: &self.agent.keeper,
            bound: self.narrowed.as_deref(),
        }
        .under_root(self.root_agent.as_deref());
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
                could_not_ask(&format!(
                    "{why}. The message may not leave unasked, so it is still begun: keep what \
                     you wrote as a draft and tell them about it"
                ))
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

/// Explicit narrowing rules set on `agent` for non-Chat services, excluding Chat-only
/// one-way rules (`sub::one_way`). `None` when nothing explicitly narrows `agent`.
pub(super) fn narrowing(agent: &Agent, _workspace: &str) -> Option<Vec<tilo_action_policy::Rule>> {
    agent.bound_rules()
}

/// Verified active root assistant id for a sub-agent's turn in the same workspace and for
/// the same keeper; `None` for a top-level agent or when the root assistant is inactive.
pub(super) fn root_agent_of(turn: &Turn) -> Option<String> {
    let root = &turn.line.as_ref()?.root;
    (root.id.0 != turn.agent.id.0 && root.is_active() && root.keeper == turn.agent.keeper)
        .then(|| root.id.0.clone())
}

/// Explicit narrowing rules for non-Chat services across `turn`'s agent and any ancestors in
/// its starter line, excluding Chat-only one-way rules (`sub::one_way`).
pub(super) fn turn_narrowing(turn: &Turn) -> Option<Vec<tilo_action_policy::Rule>> {
    let mut agents = vec![&turn.agent];
    if let Some(line) = &turn.line {
        if let Some(starter) = &line.starter
            && starter.id.0 != turn.agent.id.0
            && starter.keeper == turn.agent.keeper
        {
            agents.push(starter);
        }
        if line.root.id.0 != turn.agent.id.0
            && line.root.keeper == turn.agent.keeper
            && !agents.iter().any(|seen| seen.id.0 == line.root.id.0)
        {
            agents.push(&line.root);
        }
    }
    chain_narrowing(&agents)
}

/// Explicit narrowing rules across `agents` (an agent and its ancestors), excluding Chat-only
/// one-way rules. Returns `None` when none of the agents carry explicit narrowing rules; when
/// multiple carry explicit bounds, intersects them so no descendant exceeds an ancestor.
pub(super) fn chain_narrowing(agents: &[&Agent]) -> Option<Vec<tilo_action_policy::Rule>> {
    let layers: Vec<Vec<tilo_action_policy::Rule>> = agents
        .iter()
        .filter_map(|agent| agent.bound_rules())
        .collect();
    match layers.as_slice() {
        [] => None,
        [only] => Some(only.clone()),
        many => Some(intersect_bounds(many)),
    }
}

/// Intersect multiple explicit narrowing rule sets so an action is allowed only when every
/// narrowed layer in the starter line permits it.
fn intersect_bounds(layers: &[Vec<tilo_action_policy::Rule>]) -> Vec<tilo_action_policy::Rule> {
    use tilo_action_policy::{ActionKind, Answer, Level, Segment};

    let mut kept = Vec::new();
    for (index, layer) in layers.iter().enumerate() {
        for rule in layer {
            if rule.answer == Answer::Deny {
                kept.push(rule.clone());
                continue;
            }
            if !rule.kinds.contains(ActionKind::Outside) {
                continue;
            }
            let ends_with_named_action = matches!(
                rule.node.segments().last(),
                Some(Segment::Name(Level::Action, _))
            );
            let is_connection_node = rule.node.id_at(Level::App)
                == Some(tilo_actions::remote::person_connections::PROVIDER);
            let allowed_by_all = layers.iter().enumerate().all(|(other_index, other)| {
                if other_index == index {
                    return true;
                }
                let granted = tilo_automation::grant_answer(other, &rule.node, ActionKind::Outside);
                granted.answer == Answer::Allow
                    && (is_connection_node || !ends_with_named_action || granted.names_action)
            });
            if allowed_by_all {
                kept.push(rule.clone());
            }
        }
    }
    kept
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

    /// Whether `root_id` is still an active agent for `ask.person` in `ask.workspace`.
    async fn root_active(&self, ask: &KeptAsk, root_id: &str) -> bool {
        let mut person = [0; 32];
        if hex::decode_to_slice(&ask.person, &mut person).is_err() {
            return false;
        }
        let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let Ok(Some(kept)) = self
            .workspaces
            .read_resource::<tilo_workspace_agent::WorkspaceAgents>(&ask.workspace, (CHAT, &list))
            .await
        else {
            return false;
        };
        kept.agents.iter().any(|agent| {
            agent.id.0 == root_id
                && agent.is_active()
                && tilo_action_policy::PersonId::parse(&agent.keeper)
                    .ok()
                    .and_then(|id| id.root())
                    == Some(person)
        })
    }
}

impl Carries for Sends {
    /// Only its person's allow lets the message leave, and only while the question says what
    /// the server asked. Whatever they answered, what waited is gone after it.
    fn carry<'a>(&'a self, ask: &'a KeptAsk, answer: Answered) -> Carried<'a> {
        Box::pin(async move {
            let asked_send = serde_json::from_value::<AskedSend>(ask.kept.clone()).ok();
            let mut changed = false;
            let mut root_gone = false;
            if answer == Answered::Once {
                let words = asked_send
                    .as_ref()
                    .map(AskedSend::words)
                    .unwrap_or_default();
                changed = !self.says(ask, words.trim()).await;
                if !changed
                    && let Some(root_id) = asked_send
                        .as_ref()
                        .and_then(|asked| asked.named_agent.as_deref())
                {
                    root_gone = !self.root_active(ask, root_id).await;
                }
            }
            let allowed = answer == Answered::Once && !changed && !root_gone;
            let left = self.workspaces.send_asked(ask, allowed).await.map(drop);
            if changed {
                return Err(CHANGED.to_owned());
            }
            if root_gone {
                return Err(
                    "This assistant is no longer active for that account, so nothing was sent."
                        .to_owned(),
                );
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

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        reason = "in unit tests panicking is the assertion"
    )]

    use tilo_action_policy::{ActionKind, Answer, KindSet, Rule, TreePath};
    use tilo_actions::remote::person_accounts;
    use tilo_resource::Atom;
    use tilo_workspace_agent::{Agent, AgentState, Bound, sub::Sub};

    use super::super::{Ask, Line, Turn};
    use super::{chain_narrowing, narrowing, root_agent_of, turn_narrowing};

    fn make_agent(id: &str, keeper: &str, sub_of: Option<&str>) -> Agent {
        Agent {
            id: Atom(id.to_owned()),
            name: id.to_owned(),
            keeper: keeper.to_owned(),
            state: AgentState::Active,
            sub: sub_of.map(|starter| Sub {
                started_by: starter.to_owned(),
                ..Sub::default()
            }),
            ..Agent::default()
        }
    }

    fn make_turn(agent: Agent, line: Option<Line>) -> Turn {
        Turn {
            person: agent.keeper.clone(),
            agent,
            ask: Ask::Message {
                part: String::new(),
                message: String::new(),
                attempt: 0,
            },
            conversation: String::new(),
            cause: String::new(),
            request: String::new(),
            asked: String::new(),
            fold: None,
            typed: false,
            line,
            worked: Vec::new(),
        }
    }

    fn mail_rule(workspace: &str, action: &str, answer: Answer) -> Bound {
        let rule = Rule::new(
            TreePath::workspace(workspace)
                .app(person_accounts::PROVIDER)
                .kind(person_accounts::RESOURCE)
                .resource(person_accounts::RESOURCE)
                .action(action),
            KindSet::of(ActionKind::Outside),
            answer,
        );
        Bound::of(&rule)
    }

    #[test]
    fn sub_agent_without_explicit_bounds_is_not_narrowed_by_chat_one_way() {
        let root = make_agent("scout", "ada", None);
        let worker = make_agent("sub-emails", "ada", Some("scout"));
        let turn = make_turn(
            worker.clone(),
            Some(Line {
                root: root.clone(),
                starter: Some(root),
                from_starter: true,
            }),
        );

        assert!(narrowing(&worker, "ws-1").is_none());
        assert!(turn_narrowing(&turn).is_none());
        assert_eq!(root_agent_of(&turn).as_deref(), Some("scout"));
    }

    #[test]
    fn sub_agent_inherits_ancestor_bounds_and_intersects_with_own_bounds() {
        let mut root = make_agent("scout", "ada", None);
        root.bound = vec![
            mail_rule("ws-1", person_accounts::LIST, Answer::Allow),
            mail_rule("ws-1", person_accounts::FETCH, Answer::Allow),
        ];
        let mut worker = make_agent("sub-emails", "ada", Some("scout"));

        // Unbounded sub-agent under narrowed root inherits root's explicit bounds.
        let inherited = chain_narrowing(&[&worker, &root]).expect("inherits root bounds");
        assert_eq!(inherited.len(), 2);

        // When both carry explicit bounds, only actions allowed by both survive.
        worker.bound = vec![
            mail_rule("ws-1", person_accounts::LIST, Answer::Allow),
            mail_rule("ws-1", person_accounts::SEND, Answer::Allow),
        ];
        let intersected = chain_narrowing(&[&worker, &root]).expect("intersected bounds");
        let list_node = TreePath::workspace("ws-1")
            .app(person_accounts::PROVIDER)
            .kind(person_accounts::RESOURCE)
            .resource(person_accounts::RESOURCE)
            .action(person_accounts::LIST);
        let fetch_node = TreePath::workspace("ws-1")
            .app(person_accounts::PROVIDER)
            .kind(person_accounts::RESOURCE)
            .resource(person_accounts::RESOURCE)
            .action(person_accounts::FETCH);
        let send_node = TreePath::workspace("ws-1")
            .app(person_accounts::PROVIDER)
            .kind(person_accounts::RESOURCE)
            .resource(person_accounts::RESOURCE)
            .action(person_accounts::SEND);

        assert_eq!(
            tilo_automation::grant_answer(&intersected, &list_node, ActionKind::Outside).answer,
            Answer::Allow
        );
        assert_eq!(
            tilo_automation::grant_answer(&intersected, &fetch_node, ActionKind::Outside).answer,
            Answer::Deny
        );
        assert_eq!(
            tilo_automation::grant_answer(&intersected, &send_node, ActionKind::Outside).answer,
            Answer::Deny
        );
    }

    #[test]
    fn retired_or_other_keeper_root_is_not_inherited() {
        let mut retired_root = make_agent("scout", "ada", None);
        retired_root.state = AgentState::Retired;
        let worker = make_agent("sub-emails", "ada", Some("scout"));
        let turn_retired = make_turn(
            worker.clone(),
            Some(Line {
                root: retired_root,
                starter: None,
                from_starter: true,
            }),
        );
        assert_eq!(root_agent_of(&turn_retired), None);

        let other_root = make_agent("other", "bo", None);
        let turn_other = make_turn(
            worker,
            Some(Line {
                root: other_root,
                starter: None,
                from_starter: true,
            }),
        );
        assert_eq!(root_agent_of(&turn_other), None);
    }
}
