//! What an agent on this server asks its person before something is done.
//!
//! Code of this server that needs a person's yes for one of their agents (using a vault item,
//! sending mail, connecting an account) keeps what it would do ([`Workspaces::keep_ask`]) and
//! asks through the system's one way of asking: the person's own devices show the question in
//! the asking view, where they allow it once, always or not
//! ([`Workspaces::ask_person_confirmation`]). The tool call that asked ends at once, and the
//! turn goes on. On the answer this server does what it kept, never what the question says:
//! `always` first lets exactly what was asked stand for that person as a workspace grant
//! ([`crate::workspaces::person_asking`]). An answer is carried out at most once: the ask is
//! claimed before anything is done.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::remote::Caller;
use tilo_actions::remote::person_asking::{Asked, Words};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_workspace_agent::Agent;

use super::ServerAgents as Agents;
use crate::workspaces::Workspaces;
use crate::workspaces::person_asking::Allowing;
use crate::workspaces::person_asking::kept_asks::{KeptAsk, KeptAskKind, NewAsk};

/// How long a person's devices keep asking before the question lapses as a no.
const ASKED_FOR_NS: u64 = 7 * 24 * 60 * 60 * 1_000_000_000;

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

    /// The words `ask` was put to its person with, where its carrier keeps
    /// them: a yes to a question that says anything else is carried out as a
    /// no. `None` where they are not kept.
    fn asked_with(&self, _ask: &KeptAsk) -> Option<String> {
        None
    }
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

/// Where a turn's question was put, and for which turn: kept with the ask,
/// so the server finds its way back after a restart.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct AskedIn {
    /// The conversation the turn is in, by its address in the workspace;
    /// empty when no turn asked.
    pub(super) conversation: String,
    /// The ask that caused the turn.
    pub(super) cause: String,
    /// The agent session the turn's changes are grouped by.
    pub(super) session: String,
}

/// Where and for which turn `caller` asks, as a kept ask records it.
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

/// The person a kept ask names by their root key, hex.
pub(super) fn person_of(hex_key: &str) -> Option<[u8; 32]> {
    let mut person = [0; 32];
    hex::decode_to_slice(hex_key, &mut person).ok()?;
    Some(person)
}

/// One shared settle loop over any kept-ask kind:
/// - skips asks currently being carried out (`started && !interrupted`);
/// - hands asks interrupted by a server restart (`interrupted`) to `on_interrupted`;
/// - hands waiting asks (`!started`) to `on_waiting`.
pub(super) async fn settle_kept<K, I, IFut, W, WFut>(
    open: Vec<K>,
    mut on_interrupted: I,
    mut on_waiting: W,
) where
    K: KeptAskKind,
    I: FnMut(K) -> IFut,
    IFut: Future<Output = ()>,
    W: FnMut(K) -> WFut,
    WFut: Future<Output = ()>,
{
    for ask in open {
        if ask.started() && !ask.interrupted() {
            // Its answer is being carried out now.
            continue;
        }
        if ask.interrupted() {
            on_interrupted(ask).await;
        } else {
            on_waiting(ask).await;
        }
    }
}

/// Ask `person` `words` for `agent` of `workspace` through the system's one way of asking, and
/// wait for their answer: allow once, always (only where `always` may stand), or not. A question
/// that lapsed or that nobody could be asked is a no.
pub(super) async fn confirm(
    workspaces: &Workspaces,
    person: [u8; 32],
    (workspace, agent): (&str, &Agent),
    (title, named): (&str, &Words),
    always: bool,
) -> Answered {
    let mut id = [0_u8; 32];
    tilo_failure::ignored!(
        "a question without random bytes is still one question",
        getrandom::fill(&mut id).ok()
    );
    let question = PendingConfirmation {
        id,
        agent_id: agent.id.0.clone(),
        agent_label: agent.name.clone(),
        title: title.to_owned(),
        action_label: if named.action.is_empty() {
            title.to_owned()
        } else {
            named.action.clone()
        },
        resource_label: named.on.clone(),
        workspace: workspace.to_owned(),
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(ASKED_FOR_NS),
        allow_always: always,
        ..PendingConfirmation::default()
    };
    match workspaces.ask_person_confirmation(person, question).await {
        Ok(ConfirmationAnswer::AllowAlways) => Answered::Always,
        Ok(ConfirmationAnswer::AllowOnce) => Answered::Once,
        Ok(_) => Answered::No,
        Err(error) => {
            tracing::info!(%workspace, %error, "a person could not be asked, so nothing is done");
            Answered::No
        }
    }
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

    /// Ask a person, for one of the agents this server has for them, what
    /// `asking` says: a question in the conversation, as that agent, that
    /// waits until they answer it, however long that takes, also over a
    /// restart of this server. Their answer reaches the carrier `asking`
    /// names once ([`Carries::carry`]); with always, what `asking.always`
    /// names stands for them first. Asked the same again while they have not
    /// answered, the one question stands.
    ///
    /// Reached from no transport: code of this server calls it, with words of
    /// its own.
    ///
    /// # Errors
    /// The workspace's server has no such agent, the person is admitted there
    /// on no device, what always would cover is not that agent's in that
    /// workspace, too many questions wait for them already, or the question
    /// could not be put.
    pub async fn ask(&self, asking: ServerAsk) -> Result<Option<Value>, String> {
        let ServerAsk {
            workspace,
            person,
            agent,
            conversation,
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
        let workspace_id = WorkspaceId::parse(&workspace).ok_or("not a workspace id")?;
        let agents = self.agents(&workspace).await?;
        let agent = agents
            .get(&agent)
            .ok_or("this server has no such agent in that workspace")?;
        if self
            .workspaces
            .person_device(workspace_id, person)
            .await
            .is_none()
        {
            return Err("the person asked is admitted there on no device".to_owned());
        }
        let place = self
            .workspaces
            .summary(&workspace)
            .await
            .map(|summary| summary.name)
            .unwrap_or_default();
        let _ = conversation;
        let new = NewAsk {
            workspace,
            agent: agent.id.0.clone(),
            name: agent.name.clone(),
            carrier,
            kept,
            always,
            words: Words { place, ..named },
            asked_in: serde_json::to_value(AskedIn::default())
                .map_err(|error| error.to_string())?,
        };
        let Some(kept) = self.workspaces.keep_ask(&person, new)? else {
            return Ok(None);
        };
        // Asked on the person's own devices, for as long as they take: the call that asked
        // ends now, and the answer is carried out when it comes.
        tokio::spawn(self.clone().ask_kept(person, agent.clone(), kept, words.trim().to_owned()));
        Ok(None)
    }

    /// Ask again what the agents of `workspace` asked their people and nobody answered yet,
    /// as after a restart of this server: a question is put again on the person's devices,
    /// and its answer carried out as it would have been. An answer that was being carried out
    /// when this server stopped is carried out by nobody, and its ask is forgotten, as is one
    /// whose question would have lapsed by now or whose agent the workspace no longer has (a
    /// vault use stays kept).
    pub(crate) async fn settle_asks(&self, workspace: &str) {
        // Where a person's password for a calendar or a mail account is wanted and this process
        // does not ask them for it, they are asked again.
        self.settle_connects(workspace).await;
        self.settle_mail_connects(workspace).await;

        let open_asks = self.workspaces.open_asks(workspace);
        let open_uses = self.workspaces.open_use_asks(workspace);
        if open_asks.is_empty() && open_uses.is_empty() {
            return;
        }
        let agents = self.agents(workspace).await.unwrap_or_default();
        settle_kept(
            open_asks,
            |ask| {
                self.workspaces.forget_ask(&ask);
                async {}
            },
            |ask| {
                let lapsed = ask
                    .asked_at_ms
                    .saturating_mul(1_000_000)
                    .saturating_add(ASKED_FOR_NS)
                    < tilo_clock::unix_now_ns();
                let again = person_of(&ask.person)
                    .zip(agents.get(&ask.agent).cloned())
                    .filter(|_| !lapsed)
                    .map(|(person, agent)| {
                        let title = ask.words.action.clone();
                        self.clone().ask_kept(person, agent, ask.clone(), title)
                    });
                if again.is_none() {
                    self.workspaces.forget_ask(&ask);
                }
                async move {
                    if let Some(again) = again {
                        tokio::spawn(again);
                    }
                }
            },
        )
        .await;
        // A use waits for its answer as long as it is kept: one whose agent this server does
        // not find now is left as it is, and answers that it waits.
        settle_kept(
            open_uses,
            |ask| {
                self.workspaces.use_interrupted(&ask);
                async {}
            },
            |ask| {
                let again = person_of(&ask.person)
                    .zip(agents.get(&ask.agent).cloned())
                    .map(|(person, agent)| super::vault_uses::ask_use(self.clone(), person, agent, ask));
                async move {
                    if let Some(again) = again {
                        tokio::spawn(again);
                    }
                }
            },
        )
        .await;
    }

    /// Ask `person` what `kept` keeps, in the words `title`, for `agent`, and carry out their
    /// answer once it comes ([`Self::carry_answer`]).
    async fn ask_kept(self, person: [u8; 32], agent: Agent, kept: KeptAsk, title: String) {
        let answer = confirm(
            &self.workspaces,
            person,
            (&kept.workspace, &agent),
            (&title, &kept.words),
            kept.always.is_some(),
        )
        .await;
        self.carry_answer(&kept.workspace.clone(), &agent, &kept, answer)
            .await;
    }

    /// Settle every open device-password ask of kind `K` through [`settle_kept`]:
    /// interrupted asks are forgotten and reported to their agent; waiting asks
    /// are asked again (as after a restart).
    pub(super) async fn settle_password_asks<K, Fut>(
        &self,
        open: Vec<K>,
        interrupted_words: &'static str,
        forget: impl Fn(&Workspaces, &K) + Send + Sync,
        named: impl Fn(&K) -> String + Send + Sync,
        ask_and_carry: impl Fn(Self, K) -> Fut + Send + Sync,
    ) where
        K: KeptAskKind + super::calendar_connect::PutAsk + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        settle_kept(
            open,
            |ask| {
                forget(&self.workspaces, &ask);
                let this = self.clone();
                let title = named(&ask);
                async move {
                    tokio::spawn(async move {
                        this.tell_of_password(
                            &ask,
                            &title,
                            (super::calendar_connect::TYPED, interrupted_words),
                        )
                        .await;
                    });
                }
            },
            |ask| {
                let this = self.clone();
                let fut = ask_and_carry(this, ask);
                async move {
                    tokio::spawn(fut);
                }
            },
        )
        .await;
    }

    /// The person `person_hex` names, and the agent's callers for the turn `asked_in` names:
    /// within that turn, and outside it. None when the person is admitted on no device.
    pub(super) async fn asker_of(
        &self,
        workspace_id: WorkspaceId,
        person_hex: &str,
        asked_in: &Value,
        agent: &Agent,
    ) -> Option<([u8; 32], Caller, Caller)> {
        let person = person_of(person_hex)?;
        self.workspaces.person_device(workspace_id, person).await?;
        let asked_in: AskedIn = serde_json::from_value(asked_in.clone()).ok()?;
        let outside = self
            .agent_caller(workspace_id, agent, person)
            .await
            .ok()?
            .in_agent_session(asked_in.session);
        let turn = outside
            .clone()
            .on_roster_turn(tilo_action_policy::turn::Turn::new(
                asked_in.conversation,
                asked_in.cause,
            ));
        Some((person, turn, outside))
    }

    /// Carry `answer`, the person's own, out for `kept`, asked for `agent` of `workspace`:
    /// claimed first, so it is carried out once; what was asked lets stand where they said
    /// always; then what this server kept is done.
    async fn carry_answer(&self, workspace: &str, agent: &Agent, kept: &KeptAsk, answer: Answered) {
        let Some(workspace_id) = WorkspaceId::parse(workspace) else {
            return;
        };
        let Some((person, turn, _)) = self
            .asker_of(workspace_id, &kept.person, &kept.asked_in, agent)
            .await
        else {
            self.workspaces.forget_ask(kept);
            return;
        };
        let Ok(claimed) = self.workspaces.claim_ask(&kept.ask) else {
            return;
        };
        if answer == Answered::Always
            && let Some(scope) = claimed.always.clone()
        {
            let allowing = Allowing {
                scope,
                name: claimed.name.clone(),
                words: claimed.words.clone(),
                asked_by: None,
            };
            if let Err(error) = self.workspaces.let_always(&person, allowing) {
                tracing::warn!(%workspace, %error, "an always allowed ask was not kept as always allowed");
            }
        }
        if let Err(error) = self.carried(workspace_id, &turn, &claimed, answer).await {
            tracing::warn!(%workspace, %error, "what a person allowed was not done");
        }
        self.workspaces.forget_ask(&claimed);
    }

    /// Carry `answer` out for `ask` through the carrier its word names: what it answered,
    /// where it answered anything.
    async fn carried(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        ask: &KeptAsk,
        answer: Answered,
    ) -> Result<Option<Value>, String> {
        if let Some(carrier) = self.carriers.lock_recover().get(&ask.carrier).cloned() {
            return carrier.carry(ask, answer).await;
        }
        let _ = (workspace_id, caller);
        Err("Nothing on this server carries that answer out".to_owned())
    }
}
