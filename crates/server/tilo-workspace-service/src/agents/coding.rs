//! Coding agents among the agents an engine works.
//!
//! A coding agent is a sub-agent whose work a coding harness (Claude Code) does on a repository
//! ([`tilo_workspace_agent::coding`]). Everything else about it is the sub-agent's and is done by
//! the sub-agents' part ([`super::sub_agents`]): its job and its starter's notes are written into
//! its own conversation, its turns are found there like every agent's, its item says how it
//! stands, a stop on its item or on its message ends the turn that is going, and its starter is
//! told how work it gave ended.
//!
//! What differs is who answers a turn. No model loop of the engine does: the turn's words are
//! handed to a run of its harness ([`run::Run`]) on this computer, in the agent's own working
//! copy and branch, as the harness's next message. What its person writes while it works
//! reaches the same process at once; their stop ends it and everything it started. Once it
//! finished everything it was handed, its input is closed and it ends; the turn's reply gets
//! what it said. Its next words start it again, taking its conversation up where it stood by the
//! harness's own id for it, which its item keeps (`sub-agent-coding`, written here as the
//! sub-agent).
//!
//! Its model is the workspace's, for the sub-agent's kind of work, asked through the run's own
//! loopback endpoint ([`endpoint`]): the key never reaches the harness, and what it costs is
//! counted on the day of the person's own agent its line of starters ends at, as every turn's
//! is. Tilo's MCP server answers it as the sub-agent's turn: what the turn may read and write
//! holds for its harness as it holds for a model.
//!
//! A harness runs programs on the computer it is started on, so it is started only where its
//! person said so: for words they wrote to it themselves, or where they let coding agents start
//! unasked in that workspace (a lasting yes to [`PROVIDER`]'s `start`). Its job alone starts
//! nothing without that: it answers that it begins when its person says so, and their next
//! words start it with its job first. And only an engine told it may start harnesses here
//! ([`super::ServerAgents::with_coding`]) takes a coding agent's turn.

mod endpoint;
pub mod harness;
mod model_endpoint;
mod run;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::remote::person_asking::{ActionName, Asked};
use tilo_lock::MutexRecover as _;
use tilo_mcp::McpServer;
use tilo_workspace_agent::coding::{CodingHarness, CodingProcess};
use tilo_workspace_agent::{Agent, WorkspaceAgents, workspace_agents as verbs};

use super::{Ask, Outcome, Reply, ServerAgents, Turn, TurnScope, TurnUsage, going};
use super::{steps, sub_agents, summary, today};
pub use harness::{ClaudeCode, Harness};
use harness::{Says, Start};
use run::{Ended, Ending, Heard, Run};

/// The server's own provider a start of a harness is named under, where a person lets it stay.
pub const PROVIDER: &str = "tilo.coding";
/// The kind of thing started.
const KIND: &str = "harness";
/// The action a person lets stay: a coding agent's harness is started.
const START: &str = "start";
/// How long a turn waits for its harness to say something before it looks for a stop and for
/// new words.
const TICK: Duration = Duration::from_millis(250);
/// How often a turn that hears nothing renews its claim, well within the minute it holds.
const RENEW: Duration = Duration::from_secs(20);
/// How long a harness whose input was closed is given to end by itself.
const ENDING_WAIT: Duration = Duration::from_secs(30);
/// What a coding agent answers when it was handed work and its person has not said it may
/// start.
const ASKS_FIRST: &str = "I have not started. Starting me runs programs on this computer and \
                          spends on your model, so I begin when you say so: write to me here \
                          and I start with my job.";

/// The coding harnesses an engine starts on its own computer, by id.
#[derive(Clone)]
pub struct Coders {
    known: BTreeMap<&'static str, Arc<dyn Harness>>,
}

impl Coders {
    /// The harnesses every engine knows: Claude Code, as installed on this computer.
    #[must_use]
    pub fn here() -> Self {
        Self {
            known: BTreeMap::new(),
        }
        .with(Arc::new(ClaudeCode::default()))
    }

    /// These, and `harness` in place of one of the same id.
    #[must_use]
    pub fn with(mut self, harness: Arc<dyn Harness>) -> Self {
        self.known.insert(harness.id(), harness);
        self
    }

    /// The harness with `id`; Claude Code when `id` is empty.
    fn named(&self, id: &str) -> Result<Arc<dyn Harness>, String> {
        let id = if id.is_empty() {
            harness::CLAUDE_CODE
        } else {
            id
        };
        self.known.get(id).cloned().ok_or_else(|| {
            format!(
                "{id:?} is no coding harness this computer starts: it starts {}.",
                self.known.keys().copied().collect::<Vec<_>>().join(", ")
            )
        })
    }
}

/// Whether `agent` is a coding agent: a sub-agent with a coding part.
pub(super) fn codes(agent: &Agent) -> bool {
    agent.sub.as_ref().is_some_and(|sub| sub.coding.is_some())
}

/// A coding agent's harness as it was started for a turn.
struct Started {
    run: Run,
    harness: Arc<dyn Harness>,
    tools: endpoint::Tools,
    since_ms: i64,
}

/// What one run's turn heard, gathered into its answer.
#[derive(Default)]
struct Answer {
    /// What the harness said for its person, in order.
    said: Vec<String>,
    /// Its last words on finishing, which stand when it said nothing else.
    finished: String,
}

impl Answer {
    fn words(&self) -> String {
        if self.said.is_empty() {
            self.finished.clone()
        } else {
            self.said.join("\n\n")
        }
    }
}

impl ServerAgents {
    /// These agents, starting coding agents' harnesses on this computer as `coders` names them.
    /// Without this a coding agent's turn is taken by another engine of its workspace, or waits.
    #[must_use]
    pub fn with_coding(mut self, coders: Coders) -> Self {
        self.coding = Some(coders);
        self
    }

    /// Whether this engine takes `agent`'s turns: any agent but a coding agent, which only an
    /// engine that starts harnesses takes.
    pub(super) fn works(&self, agent: &Agent) -> bool {
        self.coding.is_some() || !codes(agent)
    }

    /// Whether `turn`'s person said its harness may start: they wrote it themselves, or they
    /// let coding agents start unasked in this workspace.
    fn may_start(&self, workspace: &str, turn: &Turn, root: &Agent) -> bool {
        if turn.typed && turn.person == turn.agent.acts_for {
            return true;
        }
        let person = match tilo_action_policy::PersonId::parse(&turn.agent.acts_for) {
            Ok(person) => person.root(),
            Err(error) => {
                tracing::warn!(%workspace, agent = %turn.agent.id.0, %error, "a coding agent names no person by their root, so nobody lets it start unasked");
                return false;
            }
        };
        let Some(person) = person else {
            return false;
        };
        let asked = Asked {
            workspace: workspace.to_owned(),
            agent: root.id.0.clone(),
            action: ActionName {
                provider: PROVIDER.to_owned(),
                kind: KIND.to_owned(),
                action: START.to_owned(),
            },
            on: None,
        };
        self.workspaces
            .stands(&person, &asked)
            .is_some_and(|permission| {
                self.workspaces.permission_used(&person, &permission);
                true
            })
    }

    /// Say on its item how `agent`'s harness stands, as the sub-agent itself: the server that
    /// works it writes the item, as it writes how a sub-agent stands.
    async fn coding_stands(&self, workspace: &str, agent: &Agent, report: verbs::SubAgentCoding) {
        let document = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let (id, acts_for) = (agent.id.0.clone(), agent.acts_for.clone());
        let written = self
            .workspaces
            .author_application(
                workspace,
                self.workspaces.governor(),
                &tilo_agent_session::AppCatalogue::native(),
                tilo_workspace_agent::OWNER,
                &document,
                |doc| {
                    let mut saying =
                        tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
                            .as_agent_for(&id, &acts_for)
                            .in_app(tilo_workspace_agent::OWNER, "Assistant");
                    saying.set_now_ms(tilo_clock::unix_now_ms_i64());
                    let applied = saying
                        .apply(WorkspaceAgents::SUB_AGENT_CODING.with(report))
                        .map(drop)
                        .map_err(|error| error.to_string());
                    std::mem::swap(doc, saying.doc_mut());
                    applied
                },
            )
            .await;
        if let Err(error) = written {
            tracing::warn!(%workspace, agent = %agent.id.0, %error, "how a coding agent's harness stands was not written");
        }
    }

    /// [`Self::answer_going`](super::ServerAgents) for a coding agent's turn: its harness
    /// answers, not the engine's model loop.
    pub(super) async fn answer_by_harness(
        &self,
        workspace: &str,
        (scope, mut turn, mut reply, began): (TurnScope, Turn, Reply, Ask),
        going: &going::Going,
    ) -> (Ask, Result<(), String>, Option<summary::Owed>) {
        let from_starter = turn.line.as_ref().is_some_and(|line| line.from_starter);
        let root = sub_agents::payer(&turn).clone();
        if !self.may_start(workspace, &turn, &root) {
            let waits = Outcome::Stopped(ASKS_FIRST.to_owned());
            let ended = reply.finished(&waits).await;
            super::telling::turn_ended(
                &self.workspaces,
                self.events.as_ref(),
                &scope.person,
                workspace,
                &turn,
                &waits,
            )
            .await;
            self.sub_ended(&scope, &turn, &waits, from_starter).await;
            return (turn.ask, ended, None);
        }
        reply.taken().await;
        self.sub_began(&scope, &turn).await;
        let day = today();
        let (outcome, cost) = match self
            .harness_works(workspace, &scope, (&mut turn, &mut reply), going, &day)
            .await
        {
            Ok(worked) => worked,
            Err(why) => (Outcome::Failed(why), 0),
        };
        if cost > 0 {
            self.charge(workspace, &root, &day, cost).await;
        }
        let ended = reply
            .finished_with(&outcome, cost, &TurnUsage::default())
            .await;
        super::telling::turn_ended(
            &self.workspaces,
            self.events.as_ref(),
            &scope.person,
            workspace,
            &turn,
            &outcome,
        )
        .await;
        if matches!(outcome, Outcome::Cancelled) {
            self.take_back_asked(workspace, &turn, &began).await;
        }
        self.sub_ended(&scope, &turn, &outcome, from_starter).await;
        (turn.ask, ended, None)
    }

    /// Start `turn`'s harness, hand it the turn's words and what its person writes meanwhile,
    /// and follow it until it finished them all, ended or was stopped: how the turn ended, and
    /// what its model calls cost.
    ///
    /// # Errors
    /// The harness could not be started: why, in words its person reads.
    async fn harness_works(
        &self,
        workspace: &str,
        scope: &TurnScope,
        (turn, reply): (&mut Turn, &mut Reply),
        going: &going::Going,
        day: &str,
    ) -> Result<(Outcome, u64), String> {
        let agent = turn.agent.clone();
        let sub = agent.sub.clone().ok_or("a coding agent is a sub-agent")?;
        let coding = sub
            .coding
            .clone()
            .ok_or("a coding agent has a coding part")?;
        let Started {
            mut run,
            harness,
            tools,
            since_ms: started_ms,
        } = self
            .start_harness(workspace, scope, (&*turn, &*reply), going, day)
            .await?;
        // A harness that never worked is told its job first, whatever words start it.
        let mut pending = 0_usize;
        if coding.session.is_empty() && turn.request.trim() != sub.job.trim() {
            pending = pending.saturating_add(usize::from(run.tell(&sub.job)));
        }
        pending = pending.saturating_add(usize::from(run.tell(&turn.request)));
        let mut answer = Answer::default();
        let mut last_ok = true;
        let mut renewed = tilo_clock::now();
        let mut stopped = false;
        let ended = loop {
            if !stopped && going.stopped() {
                stopped = true;
                run.stop();
            }
            if !stopped {
                for newer in going.take() {
                    reply.went_on().await?;
                    *turn = newer;
                    *reply = scope.reply(turn);
                    reply.taken().await;
                    *tools.lock_recover() = Arc::new(self.tools_of(reply)?);
                    pending = pending.saturating_add(usize::from(run.tell(&turn.request)));
                }
            }
            if renewed.elapsed() >= RENEW {
                renewed = tilo_clock::now();
                if reply.kept().await.is_err() {
                    stopped = true;
                    run.stop();
                }
            }
            match run.next(TICK).await {
                None => {}
                Some(Heard::Over(ended)) => break ended,
                Some(Heard::Said(Says::Session(session))) => {
                    self.coding_session(workspace, &agent, session).await;
                }
                Some(Heard::Said(Says::Words(words))) => answer.said.push(words),
                Some(Heard::Said(Says::Doing(what))) => {
                    reply.notes(&what);
                }
                Some(Heard::Said(Says::Finished { ok, words })) => {
                    pending = pending.saturating_sub(1);
                    last_ok = ok;
                    answer.finished = words;
                    if pending == 0 && !stopped {
                        // Everything it was handed is done: it ends once its input does.
                        run.close();
                        let ending = tokio::time::timeout(ENDING_WAIT, until_over(&mut run)).await;
                        break if let Ok(ended) = ending {
                            ended
                        } else {
                            run.stop();
                            until_over(&mut run).await
                        };
                    }
                }
            }
        };
        let spent = run.spent_micros();
        drop(run);
        let outcome = if going.stopped() {
            Outcome::Cancelled
        } else {
            outcome_of(&harness, &ended, pending, (last_ok, &answer))
        };
        self.coding_ended(workspace, (&agent, &coding), started_ms, &ended, spent)
            .await;
        Ok((outcome, spent))
    }

    /// Say on `agent`'s item the session its harness works in, which its next start resumes.
    async fn coding_session(&self, workspace: &str, agent: &Agent, session: String) {
        self.coding_stands(
            workspace,
            agent,
            verbs::SubAgentCoding {
                agent: tilo_resource::Id::from_raw(agent.id.0.clone()),
                process: None,
                session: Some(session),
                spent_micros: None,
                harness: None,
            },
        )
        .await;
    }

    /// Say on `agent`'s item how its harness's process ended, and what its model calls cost.
    async fn coding_ended(
        &self,
        workspace: &str,
        (agent, coding): (&Agent, &tilo_workspace_agent::Coding),
        started_ms: i64,
        ended: &Ended,
        spent: u64,
    ) {
        self.coding_stands(
            workspace,
            agent,
            verbs::SubAgentCoding {
                agent: tilo_resource::Id::from_raw(agent.id.0.clone()),
                process: Some(CodingProcess {
                    started_ms,
                    ended_ms: Some(tilo_clock::unix_now_ms_i64()),
                    ended: ended.how.word().to_owned(),
                    exit: ended.exit,
                    words: ended.words.clone(),
                    stopped_by: String::new(),
                }),
                session: None,
                spent_micros: Some(coding.spent_micros.saturating_add(spent)),
                harness: None,
            },
        )
        .await;
    }

    /// Start the harness of `turn`'s coding agent: the turn's model behind its endpoint, the
    /// tools the turn may use, and its process, said on the agent's item as started.
    ///
    /// # Errors
    /// The harness could not be started: why, in words its person reads.
    async fn start_harness(
        &self,
        workspace: &str,
        scope: &TurnScope,
        (turn, reply): (&Turn, &Reply),
        going: &going::Going,
        day: &str,
    ) -> Result<Started, String> {
        let coders = self
            .coding
            .clone()
            .ok_or("This computer starts no coding agents.")?;
        let agent = turn.agent.clone();
        let coding = agent
            .sub
            .as_ref()
            .and_then(|sub| sub.coding.clone())
            .ok_or("a coding agent has a coding part")?;
        let harness = coders.named(&coding.harness)?;
        let model = self
            .model_for_turn(
                (workspace, scope.workspace_id),
                turn,
                day,
                going.stop_flag(),
            )
            .await?;
        let model = tokio::task::spawn_blocking(move || model())
            .await
            .ok()
            .flatten()
            .ok_or("This workspace has no model its agents can work with.")?;
        let tools = Arc::new(Mutex::new(Arc::new(self.tools_of(reply)?)));
        let start = Start {
            session: (!coding.session.is_empty()).then(|| coding.session.clone()),
            instruction: agent.instruction.clone(),
        };
        let serves = endpoint::Serves {
            model,
            key: format!("{}:{}", agent.id.0, turn.cause),
            tools: Arc::clone(&tools),
            runtime: tokio::runtime::Handle::current(),
        };
        let run = {
            let (harness, coding) = (Arc::clone(&harness), coding.clone());
            tokio::task::spawn_blocking(move || Run::start(harness, &coding, &start, serves))
                .await
                .map_err(|error| error.to_string())??
        };
        let started_ms = tilo_clock::unix_now_ms_i64();
        self.coding_stands(
            workspace,
            &agent,
            verbs::SubAgentCoding {
                agent: tilo_resource::Id::from_raw(agent.id.0.clone()),
                process: Some(CodingProcess {
                    started_ms,
                    ..CodingProcess::default()
                }),
                session: None,
                spent_micros: None,
                harness: Some(CodingHarness {
                    id: harness.id().to_owned(),
                    name: harness.name().to_owned(),
                }),
            },
        )
        .await;
        Ok(Started {
            run,
            harness,
            tools,
            since_ms: started_ms,
        })
    }

    /// The agent tools of the turn `reply` answers, as its harness reaches them.
    fn tools_of(&self, reply: &Reply) -> Result<McpServer<super::WorkspaceActionProvider>, String> {
        let _ = self;
        McpServer::for_turn(reply.provider.clone(), reply.caller.clone())
            .map(|server| server.with_observation(steps::observed()))
            .map_err(|error| error.to_string())
    }
}

/// What a run says once it is over.
/// How a turn its harness was not stopped in ended: answered when it finished all it was handed
/// and the last went well, else failed, in words its person reads.
fn outcome_of(
    harness: &Arc<dyn Harness>,
    ended: &Ended,
    pending: usize,
    (last_ok, answer): (bool, &Answer),
) -> Outcome {
    if pending == 0 && last_ok {
        return Outcome::Answered(answer.words());
    }
    if pending == 0 {
        return Outcome::Failed(format!(
            "{} could not finish: {}",
            harness.name(),
            answer.words()
        ));
    }
    match ended.how {
        Ending::Stopped => Outcome::Failed("another device took this turn".to_owned()),
        Ending::Finished => Outcome::Failed(format!(
            "{} ended before it finished what it was asked.",
            harness.name()
        )),
        Ending::Failed | Ending::Lost => Outcome::Failed(failed(harness, ended)),
    }
}

async fn until_over(run: &mut Run) -> Ended {
    loop {
        match run.next(TICK).await {
            Some(Heard::Over(ended)) => return ended,
            Some(Heard::Said(_)) | None => {}
        }
    }
}

/// Why a harness that failed failed, in words its person reads.
fn failed(harness: &Arc<dyn Harness>, ended: &Ended) -> String {
    match (ended.how, ended.words.trim()) {
        (Ending::Lost, "") => format!("{} went away without saying why.", harness.name()),
        (_, "") => format!(
            "{} ended with status {}.",
            harness.name(),
            ended.exit.unwrap_or(-1)
        ),
        (_, words) => format!("{} failed: {words}", harness.name()),
    }
}
