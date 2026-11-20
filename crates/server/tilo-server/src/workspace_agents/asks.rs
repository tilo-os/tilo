//! Agent approval requests routed through the server's single pending-confirmation
//! path ([`Workspaces::ask_person_confirmation`]) and `tilo-action-policy` workspace grants.
//!
//! No approval cards are posted to Chat and no Chat polling loop reads answers back.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use collaboration::permissions::MemberId;
use serde_json::{Map, Value};
use tilo_actions::remote::person_asking::{Asked, Words};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};

use super::{Ask, HostAgents as Agents, Turn};
use crate::workspaces::Workspaces;
use crate::workspaces::person_asking::kept_asks::KeptAsk;

/// How long the person has to answer a pending confirmation.
const QUESTION_WAIT_NS: u64 = 2 * 60 * 1_000_000_000;

/// What a person answered about an ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answered {
    /// Do it this once.
    Once,
    /// Do it, and from now on without asking: a workspace policy grant stands.
    Always,
    /// Do not do it.
    No,
}

/// The answer to an ask, once it is carried out.
pub type Carried<'a> = Pin<Box<dyn Future<Output = Result<Option<Value>, String>> + Send + 'a>>;

/// What carries out a person's answer to an ask code of this server put under
/// a word of its own.
pub trait Carries: Send + Sync {
    /// Carry `answer`, the person's own, out for `ask`.
    ///
    /// # Errors
    /// Why it could not be done, in words its person reads.
    fn carry<'a>(&'a self, ask: &'a KeptAsk, answer: Answered) -> Carried<'a>;
}

/// The carriers of this server, by their word.
pub(super) type Carriers = Arc<std::sync::Mutex<BTreeMap<String, Arc<dyn Carries>>>>;

/// This server's one way of asking a person, as a turn's provider keeps it.
#[derive(Clone)]
pub(super) struct Asker {
    pub(super) workspaces: Workspaces,
    ask: Arc<dyn Fn(ServerAsk) -> Carried<'static> + Send + Sync>,
}

impl Asker {
    /// Ask a person what `asking` says.
    ///
    /// # Errors
    /// Why the ask could not be put or carried out.
    pub(super) async fn ask(&self, asking: ServerAsk) -> Result<Option<Value>, String> {
        (self.ask)(asking).await
    }
}

/// What code of this server asks a person, for one of the agents this server
/// has for them.
#[derive(Clone, Debug)]
pub struct ServerAsk {
    pub workspace: String,
    pub person: [u8; 32],
    pub agent: String,
    pub conversation: String,
    pub words: String,
    pub carrier: String,
    pub kept: Value,
    pub always: Option<Asked>,
    pub named: Words,
}

/// The conversation a turn's message belongs to, or for an issue the person's
/// direct conversation with the agent.
pub(super) fn conversation_id_of(conversation: &str, person: &str, agent: &str) -> String {
    let conversation_ids =
        chat::summons::conversation_address(&tilo_resource::Id::from_raw(String::new()));
    conversation
        .strip_prefix(&conversation_ids)
        .filter(|conversation_id| !conversation_id.is_empty())
        .map_or_else(
            || {
                chat::Chat::direct_id([MemberId::person(person), MemberId::agent(agent)])
                    .to_string()
            },
            str::to_owned,
        )
}

impl Agents {
    /// Carry the answers to asks put under the word `carrier` out through `by`.
    pub fn carries(&self, carrier: &str, by: Arc<dyn Carries>) {
        self.carriers.lock_recover().insert(carrier.to_owned(), by);
    }

    /// [`Self::ask`] as a turn's provider keeps it.
    pub(super) fn as_asker(&self) -> Asker {
        let agents = self.clone();
        Asker {
            workspaces: self.workspaces.clone(),
            ask: Arc::new(move |asking| {
                let agents = agents.clone();
                Box::pin(async move { agents.ask(asking).await })
            }),
        }
    }

    /// Ask a person through the broker's single pending-confirmation path
    /// ([`Workspaces::ask_person_confirmation`]) and carry out their answer
    /// through the registered carrier.
    ///
    /// # Errors
    /// The ask is invalid, the agent does not exist, or carrying out the answer failed.
    pub async fn ask(&self, asking: ServerAsk) -> Result<Option<Value>, String> {
        let ServerAsk {
            workspace,
            person,
            agent,
            conversation: _,
            words,
            carrier,
            kept,
            always,
            named,
        } = asking;
        if carrier.is_empty() {
            return Err("an ask names the carrier of its answer by a word of its own".to_owned());
        }
        if words.trim().is_empty() {
            return Err("an ask says what is asked".to_owned());
        }
        if always.as_ref().is_some_and(|scope| {
            scope.workspace != workspace || scope.agent != agent || !scope.is_named()
        }) {
            return Err("what always covers is that agent's, in that workspace".to_owned());
        }
        let agents = self.agents(&workspace).await?;
        let agent = agents
            .get(&agent)
            .ok_or("this server has no such agent in that workspace")?;
        let place = self
            .workspaces
            .summary(&workspace)
            .await
            .map(|summary| summary.name)
            .unwrap_or_default();
        let ask = KeptAsk {
            ask: hex::encode(rand::random::<[u8; 16]>()),
            workspace: workspace.clone(),
            person: hex::encode(person),
            agent: agent.id.0.clone(),
            name: agent.name.clone(),
            carrier: carrier.clone(),
            kept,
            always,
            words: Words { place, ..named },
            asked_in: Value::Object(Map::new()),
            started: true,
            interrupted: false,
            asked_at_ms: tilo_clock::unix_now_ms(),
        };
        let question = PendingConfirmation {
            id: rand::random(),
            agent_id: ask.agent.clone(),
            agent_label: ask.name.clone(),
            title: String::new(),
            action_label: if ask.words.action.is_empty() {
                words.trim().to_owned()
            } else {
                ask.words.action.clone()
            },
            resource_label: ask.words.on.clone(),
            arguments: vec![("Request".to_owned(), words)],
            risk: "sensitive".to_owned(),
            kind: "outside".to_owned(),
            undoable: false,
            allow_always: ask.always.is_some() && self.workspaces.governs(&workspace),
            workspace,
            expires_at_ns: tilo_clock::unix_now_ns().saturating_add(QUESTION_WAIT_NS),
            page: None,
            ..PendingConfirmation::default()
        };
        let answer = match self
            .workspaces
            .ask_person_confirmation(person, question)
            .await
        {
            Ok(ConfirmationAnswer::AllowOnce) => Answered::Once,
            Ok(ConfirmationAnswer::AllowAlways) => Answered::Always,
            Ok(ConfirmationAnswer::Deny | ConfirmationAnswer::Reply(_)) | Err(_) => Answered::No,
        };
        let Some(runner) = self.carriers.lock_recover().get(&carrier).cloned() else {
            return Err("there is no handler for this approval".to_owned());
        };
        runner.carry(&ask, answer).await
    }

    /// Settle pending connection setup prompts for `workspace`.
    pub(super) async fn settle_asks(&self, workspace: &str) {
        Box::pin(self.settle_connects(workspace)).await;
        Box::pin(self.settle_mail_connects(workspace)).await;
    }

    /// No-op: approval confirmations are no longer posted into Chat conversations.
    #[expect(
        clippy::unused_async,
        reason = "no-op stub preserving caller signature"
    )]
    pub(super) async fn take_back_asked(&self, _workspace: &str, _turn: &Turn, _ask: &Ask) {}
}
