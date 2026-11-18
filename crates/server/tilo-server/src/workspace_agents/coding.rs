//! Coding agents among the agents worked here.
//!
//! A coding agent is a sub-agent whose work a coding harness does in the workspace's machine
//! ([`tilo_workspace_agent::coding`]). Everything else about it is the sub-agent's and is done
//! by the sub-agents' part ([`super::sub_agents`]): its job and its starter's notes are written
//! into its own conversation, its turns are found there like every agent's, its item says how
//! it stands, a stop on its item or on its message ends the turn that is going, and its starter
//! is told how work it gave ended.
//!
//! What differs is who answers a turn. No model loop does: the turn's words are handed to its
//! harness ([`CodingAgents`]) as the harness's next message, what the harness says is written
//! into the conversation as it says it, and once it has finished everything it was handed, the
//! turn's reply gets its last words. What its person writes meanwhile reaches the same process.
//! Its item says which harness works, the harness's own id for the conversation, its process
//! and what it spent (`sub-agent-coding`, written here as the sub-agent, by no tool of a turn).
//!
//! A harness is started only where its person said so: for words they wrote to it themselves
//! ([`Permit::Once`]), or where they let coding agents start unasked in that workspace's
//! machine ([`Permit::Lasting`], read from their own store each time: [`Workspaces::stands`]).
//! Its job alone starts nothing without that: it answers that it begins when its person says
//! so, and their next words start it with the job first.
//!
//! What its model calls cost is counted where every turn's is: on the day of the person's own
//! agent its line of starters ends at, in the person's own store, which refuses the call that
//! would pass the day's limit. Tilo's own MCP server answers its harness only while a turn of
//! it goes, as that turn's caller through that turn's own action service: what a sub-agent's
//! turn may read and write holds for its harness as it holds for a model.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use tilo_actions::remote::person_asking::{ActionName, Asked};
use tilo_lock::MutexRecover as _;
use tilo_mcp::McpServer;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_resource::Id;
use tilo_workspace_agent::coding::{CodingHarness, CodingProcess, MACHINE};
use tilo_workspace_agent::{Agent, Stands, Sub, WorkspaceAgents, workspace_agents as verbs};

use super::{Ask, CHAT, Outcome, Reply, Turn, TurnScope, going, invoke_named, sub_agents, today};
use crate::coding_agents::harness::Harnesses;
use crate::coding_agents::run::{Conversation, Ended, Ending, Serves};
use crate::coding_agents::runner::{CodingAgent, CodingAgents, Machines, NotTold, Permit, Permits};
use crate::coding_agents::working_copy::WorkingCopy;
use crate::workspaces::Workspaces;
use crate::workspaces::person_model::Spender;

/// How long a turn waits for its harness to say something before it looks for a stop and for
/// new words.
const TICK: Duration = Duration::from_millis(250);
/// How long a harness that was asked to end is waited for.
const STOP_WAIT: Duration = Duration::from_secs(45);
/// The server's own provider a start in the machine is named under, where a person lets it
/// stay.
const PROVIDER: &str = "tilo.coding";
/// The action a person lets stay: a coding agent's harness is started.
const START: &str = "start";
/// What a coding agent answers when it was handed work and its person has not said it may
/// start.
const ASKS_FIRST: &str = "I have not started. Starting me spends on your model and may change \
                          the repository, so I begin when you say so: write to me here and I \
                          start with my job.";
/// What a turn says when its harness ended without saying how.
const GONE: &str = "The coding agent in the machine is gone: it ended without saying how.";
/// What Tilo's MCP server answers a harness that asks it while no turn of its agent goes.
const NO_TURN: &str = "This agent has no turn going: it acts only while it answers a message.";

/// One thing a coding agent's harness said.
enum Said {
    Session(String),
    Says(String),
    Doing(String),
    Finished(bool, String),
    Over(Ended),
}

/// The agent tools of one turn, as a harness reaches them.
type Tools = Arc<McpServer<super::WorkspaceActionProvider>>;

/// Whose day a harness's model calls are counted on, and within what limit.
#[derive(Clone, Default)]
struct Spends {
    agent: String,
    name: String,
    limit_micros: Option<u64>,
}

/// What one coding agent's harness said that nothing has taken yet, and what is kept of its
/// process between two turns.
#[derive(Default)]
struct Heard {
    said: Mutex<VecDeque<Said>>,
    woke: tokio::sync::Notify,
    /// A turn of it is going: what is said is that turn's to take.
    in_turn: AtomicBool,
    /// The agent tools of the turn that is going; none between two turns, when Tilo answers
    /// its harness nothing.
    tools: Mutex<Option<Tools>>,
    /// Whose day its model calls are counted on, as its newest turn found it.
    spends: Mutex<Spends>,
    /// When its process started, in milliseconds since the Unix epoch.
    started_ms: AtomicI64,
    /// What its item said it had spent before its process started.
    spent_before: AtomicU64,
    /// What of its process's spending is counted on its payer's day already.
    counted: AtomicU64,
    /// The most its item was told it spent in all.
    all: AtomicU64,
    /// The member who stopped it through Chat while it lived.
    stopped_by: Mutex<String>,
}

impl Heard {
    fn push(&self, said: Said) {
        self.said.lock_recover().push_back(said);
        self.woke.notify_one();
    }

    fn pop(&self) -> Option<Said> {
        self.said.lock_recover().pop_front()
    }

    /// The next thing said, waited for at most `wait`.
    async fn next(&self, wait: Duration) -> Option<Said> {
        if let Some(first) = self.pop() {
            return Some(first);
        }
        tokio::time::timeout(wait, self.woke.notified()).await.ok();
        self.pop()
    }

    fn quiet(&self) -> bool {
        self.said.lock_recover().is_empty()
    }

    fn in_turn(&self) -> bool {
        self.in_turn.load(Ordering::SeqCst)
    }
}

impl Conversation for Heard {
    fn session(&self, id: &str) {
        self.push(Said::Session(id.to_owned()));
    }

    fn says(&self, words: &str) {
        self.push(Said::Says(words.to_owned()));
    }

    fn doing(&self, what: &str) {
        self.push(Said::Doing(what.to_owned()));
    }

    fn finished(&self, ok: bool, words: &str) {
        self.push(Said::Finished(ok, words.to_owned()));
    }

    fn over(&self, ended: &Ended) {
        self.push(Said::Over(ended.clone()));
    }
}

/// What a person let stay, read from their own store on this server each time a start asks:
/// their own agent may start coding agents in this workspace's machine without asking them.
struct Leave(Workspaces);

impl Permits for Leave {
    fn stands(&self, agent: &CodingAgent) -> bool {
        let Some(person) = tilo_action_policy::PersonId::parse(&agent.person)
            .ok()
            .and_then(|person| person.root())
        else {
            return false;
        };
        let asked = Asked {
            workspace: agent.workspace.clone(),
            agent: agent.under.clone(),
            action: ActionName {
                provider: PROVIDER.to_owned(),
                kind: MACHINE.to_owned(),
                action: START.to_owned(),
            },
            on: None,
        };
        self.0.stands(&person, &asked).is_some_and(|permission| {
            self.0.permission_used(&person, &permission);
            true
        })
    }
}

/// The coding agents kept here, and what each one's harness said.
pub(super) struct Coders {
    kept: Arc<CodingAgents>,
    heard: Mutex<BTreeMap<(String, String), Arc<Heard>>>,
}

impl Coders {
    fn heard(&self, workspace: &str, agent: &str) -> Arc<Heard> {
        Arc::clone(
            self.heard
                .lock_recover()
                .entry((workspace.to_owned(), agent.to_owned()))
                .or_default(),
        )
    }

    /// What is heard of `workspace`'s coding agents, by agent.
    fn heard_in(&self, workspace: &str) -> Vec<(String, Arc<Heard>)> {
        self.heard
            .lock_recover()
            .iter()
            .filter(|((of, _), _)| of == workspace)
            .map(|((_, agent), heard)| (agent.clone(), Arc::clone(heard)))
            .collect()
    }

    /// Whether a process of `agent`'s harness lives.
    fn lives(&self, workspace: &str, agent: &str) -> bool {
        self.kept.runs(workspace, agent)
    }
}

/// What one turn of a coding agent works with.
struct Work {
    coders: Arc<Coders>,
    heard: Arc<Heard>,
    described: CodingAgent,
    serves: Serves,
    sub: Sub,
    /// The person's own agent its line of starters ends at: its spending is counted on that
    /// agent's day.
    payer: Agent,
    /// The messages its harness was handed and has not finished.
    pending: usize,
    /// What its harness said last that nothing shows yet.
    said: Option<String>,
    /// What it said that its conversation did not take as a message: the turn's answer
    /// begins with it.
    unwritten: Vec<String>,
    /// Whether a process of its harness lived when the turn began.
    lived: bool,
    /// Whether this turn handed it its job.
    told_job: bool,
}

impl Work {
    /// Write `words` into its conversation as a message of its own. Words Chat does not take
    /// there are kept for the turn's answer, so nothing it said is lost.
    async fn write(&mut self, reply: &Reply, words: &str) {
        if !post(reply, &self.sub.conversation, words).await {
            self.unwritten.push(words.trim().to_owned());
        }
    }

    /// Show what its harness said last, as a message of its own.
    async fn show(&mut self, reply: &Reply) {
        if let Some(words) = self.said.take() {
            self.write(reply, &words).await;
        }
    }

    /// `answer`, after whatever it said before that its conversation did not take.
    fn whole(&mut self, answer: String) -> String {
        let mut parts = std::mem::take(&mut self.unwritten);
        parts.push(answer);
        parts.retain(|part| !part.trim().is_empty());
        parts.join("\n\n")
    }
}

/// Whether `agent` is a coding agent: a sub-agent with a coding part.
fn codes(agent: &Agent) -> bool {
    agent.sub.as_ref().is_some_and(|sub| sub.coding.is_some())
}

/// Whether `asker`, a person by their key as an ask names them, is the person `keeper` names:
/// the one whose model a coding agent's harness asks. Only their own words are a yes to a
/// start.
fn is_keeper(keeper: &str, asker: &str) -> bool {
    let keeper = tilo_action_policy::PersonId::parse(keeper)
        .ok()
        .and_then(|keeper| keeper.root());
    let asker: Option<[u8; 32]> = hex::decode(asker)
        .ok()
        .and_then(|bytes| bytes.try_into().ok());
    keeper.is_some() && keeper == asker
}

/// Whether `turn` answers words the person who keeps its agent wrote to it themselves, under
/// no cause (`kind`): their yes to starting its harness.
fn is_their_yes(turn: &Turn, kind: Option<&str>) -> bool {
    turn.typed && kind.is_none() && is_keeper(&turn.agent.keeper, &turn.person)
}

/// The document that keeps a workspace's agents.
fn agents_document() -> String {
    collab_types::workspace_document_id(tilo_workspace_agent::KIND)
}

/// The agent tools of the turn `reply` answers, as its harness reaches them: the turn's own
/// action service and the reply's own caller.
fn tools_of(scope: &TurnScope, reply: &Reply) -> Result<Tools, String> {
    McpServer::for_turn(scope.provider.clone(), reply.caller.clone())
        .map(Arc::new)
        .map_err(|error| error.to_string())
}

/// What a harness that asks Tilo's MCP server with no turn going is answered: a request its
/// refusal, a message that asks for no answer nothing.
fn no_turn(request: &Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    Some(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": -32000, "message": NO_TURN},
    }))
}

/// Write `words` into `conversation` as a message of the agent `reply` answers as, through the
/// turn's own action service: whether Chat took them. A sub-agent's turn writes plain words
/// into its own conversation and nothing else.
async fn post(reply: &Reply, conversation: &str, words: &str) -> bool {
    let words = words.trim();
    if words.is_empty() {
        return true;
    }
    let route = DocumentRoute {
        app_id: CHAT.to_owned(),
        cluster_id: reply.workspace.clone(),
        document: String::new(),
    };
    let mut arguments = Map::new();
    arguments.insert("channel".into(), json!(conversation));
    arguments.insert("text".into(), json!(words));
    let posted = invoke_named(
        &reply.provider,
        &reply.caller,
        &route,
        &collab_types::document::chain_resource_id(&chat::parts::chain()),
        "post",
        arguments,
    )
    .await;
    if let Err(error) = &posted {
        tracing::warn!(workspace = %reply.workspace, %error, "what a coding agent said was not taken as a message of its conversation");
    }
    posted.is_ok()
}

impl super::HostAgents {
    /// These agents, working those that are coding agents in their workspaces' machines: each
    /// by the coding harness its item names, of those every server knows, started in
    /// `machines`. Without this a coding agent is worked by nobody here and waits.
    #[must_use]
    pub fn with_coding(self, machines: Arc<dyn Machines>) -> Self {
        self.with_coding_by(machines, Harnesses::known())
    }

    /// [`Self::with_coding`], each worked by one of `harnesses`.
    #[must_use]
    pub fn with_coding_by(mut self, machines: Arc<dyn Machines>, harnesses: Harnesses) -> Self {
        let permits = Arc::new(Leave(self.workspaces.clone()));
        self.coding = Some(Arc::new(Coders {
            kept: Arc::new(CodingAgents::new(machines, harnesses, permits)),
            heard: Mutex::default(),
        }));
        self
    }

    /// Whether `agent` is a coding agent whose turns are worked here.
    pub(super) fn codes(&self, agent: &Agent) -> bool {
        self.coding.is_some() && codes(agent)
    }

    /// Whether a look at `workspace` owes its coding agents anything beside their turns:
    /// something a harness said, or a harness that lives, with no turn of its agent going.
    /// Harnesses that finished long enough ago are put to rest on the way.
    pub(super) fn codes_in(&self, workspace: &str) -> bool {
        self.coding.as_ref().is_some_and(|coders| {
            coders.kept.rest();
            coders.heard_in(workspace).iter().any(|(agent, heard)| {
                !heard.in_turn() && (!heard.quiet() || coders.lives(workspace, agent))
            })
        })
    }

    /// What `workspace`'s coding agents are owed beside their turns: a stop on an agent's item
    /// ends the harness that lives with no turn going, and how a harness ended meanwhile is
    /// said on its item.
    pub(super) async fn settle_coding(&self, workspace: &str) {
        let Some(coders) = self.coding.clone() else {
            return;
        };
        let Ok(Some(known)) = self
            .read::<WorkspaceAgents>(workspace, CHAT, &agents_document())
            .await
        else {
            return;
        };
        for (id, heard) in coders.heard_in(workspace) {
            if heard.in_turn() {
                continue;
            }
            let Some(agent) = known.get(&Id::from_raw(id.clone())) else {
                continue;
            };
            let stopped = agent
                .sub
                .as_ref()
                .is_some_and(|sub| sub.stands == Stands::Stopped);
            if stopped && coders.lives(workspace, &id) {
                coders.kept.stop(workspace, &id);
            }
            self.settle(workspace, (agent, known.root_of(agent)), &heard)
                .await;
        }
    }

    /// Answer `turn` of a coding agent: its reply says it is taken, its harness works it, the
    /// reply gets its last words, its item says how it stands, and when the work came from its
    /// line of starters (or its job was handed over with its person's words), its starter is
    /// told. The ask it ended on, and whether its answer was written.
    pub(super) async fn answer_coding(
        &self,
        scope: &TurnScope,
        (mut turn, mut reply): (Turn, Reply),
        going: &going::Going,
    ) -> (Ask, Result<(), String>) {
        reply.taken().await;
        self.sub_began(&scope.workspace, &turn).await;
        let from_starter = turn.line.as_ref().is_some_and(|line| line.from_starter);
        let began = turn.ask.clone();
        let (outcome, told_job) =
            Box::pin(self.work_coding(scope, &mut turn, &mut reply, going)).await;
        let ended = reply.finished(&outcome).await;
        if matches!(outcome, Outcome::Cancelled) {
            // Its person stopped it: what it asked them and still waits is taken back.
            self.take_back_asked(&scope.workspace, &turn, &began).await;
        }
        self.sub_ended(scope, &turn, &outcome, from_starter || told_job)
            .await;
        (turn.ask, ended)
    }

    /// Work `turn` with its harness until everything handed to it is finished: how the turn
    /// ended, and whether it handed the harness its job.
    async fn work_coding(
        &self,
        scope: &TurnScope,
        turn: &mut Turn,
        reply: &mut Reply,
        going: &going::Going,
    ) -> (Outcome, bool) {
        let mut work = match self.begin(scope, turn, reply).await {
            Ok(work) => work,
            Err(why) => return (Outcome::Failed(why), false),
        };
        let outcome = self.follow(&mut work, (scope, turn, reply, going)).await;
        // Tilo answers its harness only while a turn of it goes.
        *work.heard.tools.lock_recover() = None;
        self.count(&scope.workspace, &turn.agent, &work).await;
        work.heard.in_turn.store(false, Ordering::SeqCst);
        (outcome, work.told_job)
    }

    /// What `turn` of a coding agent works with: its harness as its item describes it, what
    /// answers its endpoints, and where what it says is heard.
    async fn begin(&self, scope: &TurnScope, turn: &Turn, reply: &Reply) -> Result<Work, String> {
        let coders = self
            .coding
            .clone()
            .ok_or("no coding agents are kept here")?;
        let agent = &turn.agent;
        let sub = agent.sub.clone().ok_or("only a sub-agent codes")?;
        let coding = sub.coding.clone().ok_or("that sub-agent does not code")?;
        let workspace = scope.workspace.as_str();
        let payer = sub_agents::payer(turn).clone();
        let heard = coders.heard(workspace, &agent.id.0);
        heard.in_turn.store(true, Ordering::SeqCst);
        // What its harness said while no turn was going is settled first: only its end counts.
        self.settle(workspace, (agent, &payer), &heard).await;
        let described = CodingAgent {
            workspace: workspace.to_owned(),
            id: agent.id.0.clone(),
            harness: coding.harness.clone(),
            repository: coding.repository.clone(),
            copy: Some(WorkingCopy {
                repository: coding.repository.clone(),
                directory: coding.directory.clone(),
                branch: coding.branch.clone(),
                base: (!coding.base.is_empty()).then(|| coding.base.clone()),
            }),
            session: (!coding.session.is_empty()).then(|| coding.session.clone()),
            instruction: agent.instruction.clone(),
            person: agent.keeper.clone(),
            under: payer.id.0.clone(),
        };
        *heard.spends.lock_recover() = Spends {
            agent: payer.id.0.clone(),
            name: payer.name.clone(),
            limit_micros: payer.spend_limit_micros,
        };
        let serves = self.serves(scope, agent, &heard);
        let tools = serves.and_then(|serves| Ok((serves, tools_of(scope, reply)?)));
        let (serves, tools) = match tools {
            Ok(both) => both,
            Err(why) => {
                heard.in_turn.store(false, Ordering::SeqCst);
                return Err(why);
            }
        };
        *heard.tools.lock_recover() = Some(tools);
        Ok(Work {
            lived: coders.lives(workspace, &agent.id.0),
            coders,
            heard,
            described,
            serves,
            sub,
            payer,
            pending: 0,
            said: None,
            unwritten: Vec::new(),
            told_job: false,
        })
    }

    /// What answers the endpoints of `agent`'s harness: its person's own model, counted on the
    /// day of whoever its newest turn found it spends as, and Tilo's own MCP server as the
    /// sub-agent on the turn that is going.
    fn serves(
        &self,
        scope: &TurnScope,
        agent: &Agent,
        heard: &Arc<Heard>,
    ) -> Result<Serves, String> {
        // Its keeper pays, so their own model answers.
        let keeper: [u8; 32] = tilo_action_policy::PersonId::parse(&agent.keeper)
            .ok()
            .and_then(|keeper| keeper.root())
            .ok_or("a sub-agent works for a person")?;
        let runtime = tokio::runtime::Handle::current();
        let (workspaces, workspace, workspace_id) = (
            self.workspaces.clone(),
            scope.workspace.clone(),
            scope.workspace_id,
        );
        let (spending, asking) = (Arc::clone(heard), Arc::clone(heard));
        Ok(Serves {
            model: Arc::new(move |kind| {
                let spends = spending.spends.lock_recover().clone();
                workspaces
                    .person_model(keeper, kind)
                    .working_in(workspace_id)
                    .spending_as(Spender {
                        workspace: workspace.clone(),
                        agent: spends.agent,
                        name: spends.name,
                        day: today(),
                        limit_micros: spends.limit_micros,
                    })
            }),
            // Asked from the harness's own thread, which is none of the async ones.
            tilo: Arc::new(move |request| {
                let tools = asking.tools.lock_recover().clone();
                match tools {
                    Some(tools) => runtime.block_on(tools.handle(request)),
                    None => no_turn(&request),
                }
            }),
            language: None,
        })
    }

    /// Hand `words` to the harness of `work`, starting it where it does not live. `typed` says
    /// its person wrote them to it themselves, which is their yes to that start; any other
    /// start needs what they let stay.
    async fn tell(
        &self,
        work: &mut Work,
        (workspace, agent): (&str, &Agent),
        words: &str,
        typed: bool,
    ) -> Result<(), Outcome> {
        let permit = if typed { Permit::Once } else { Permit::Lasting };
        let (kept, described, serves, heard, words) = (
            Arc::clone(&work.coders.kept),
            work.described.clone(),
            work.serves.clone(),
            Arc::clone(&work.heard) as Arc<dyn Conversation>,
            words.to_owned(),
        );
        // Starting it may wake the workspace's machine: not a thing to wait for on an async thread.
        let told = tokio::task::spawn_blocking(move || {
            kept.tell(&described, &words, serves, heard, permit)
        })
        .await
        .map_err(|error| Outcome::Failed(format!("the coding agent was not started: {error}")))?;
        match told {
            Ok(started) => {
                work.pending = work.pending.saturating_add(1);
                if started {
                    self.began(work, workspace, agent).await;
                }
                Ok(())
            }
            Err(NotTold::NeedsYes) => Err(Outcome::Stopped(ASKS_FIRST.to_owned())),
            Err(NotTold::Failed(why)) => Err(Outcome::Failed(why)),
        }
    }

    /// A process of `agent`'s harness began: its item says when, and which harness it is.
    async fn began(&self, work: &Work, workspace: &str, agent: &Agent) {
        let now = tilo_clock::unix_now_ms_i64();
        let heard = &work.heard;
        // What its item said it had spent, or what the item was told since that was read.
        let on_item = work
            .sub
            .coding
            .as_ref()
            .map_or(0, |coding| coding.spent_micros);
        heard.spent_before.store(
            on_item.max(heard.all.load(Ordering::SeqCst)),
            Ordering::SeqCst,
        );
        heard.started_ms.store(now, Ordering::SeqCst);
        heard.counted.store(0, Ordering::SeqCst);
        heard.stopped_by.lock_recover().clear();
        let harness = work
            .coders
            .kept
            .harness(&work.described.harness)
            .map(|(id, name)| CodingHarness {
                id: id.to_owned(),
                name: name.to_owned(),
            });
        self.coder_says(workspace, agent, |report| {
            report.process = Some(CodingProcess {
                started_ms: now,
                ..CodingProcess::default()
            });
            report.harness = harness;
        })
        .await;
    }

    /// Follow `work`'s harness through `turn`: hand it the turn's words, write what it says,
    /// hand it what its person writes meanwhile, and end when it finished everything it was
    /// handed, when it is over, or when it was stopped.
    async fn follow(
        &self,
        work: &mut Work,
        (scope, turn, reply, going): (&TurnScope, &mut Turn, &mut Reply, &going::Going),
    ) -> Outcome {
        let workspace = scope.workspace.clone();
        let (words, kind) = self.asked(&workspace, turn).await;
        // Words its person wrote to it themselves, under no cause, are their yes to a start.
        let typed = is_their_yes(turn, kind.as_deref());
        // A harness that never worked is handed its job before anything else it is told.
        if !work.lived
            && work.described.session.is_none()
            && kind.as_deref() != Some(sub_agents::JOB)
        {
            let job = work.sub.job.clone();
            if let Err(outcome) = self
                .tell(work, (&workspace, &turn.agent), &job, typed)
                .await
            {
                return outcome;
            }
            work.told_job = true;
        }
        if let Err(outcome) = self
            .tell(work, (&workspace, &turn.agent), &words, typed)
            .await
        {
            return outcome;
        }
        let mut stop_asked: Option<Instant> = None;
        let mut gone = false;
        loop {
            if going.stopped() && stop_asked.is_none() {
                self.stop(work, &workspace, turn).await;
                stop_asked = Some(Instant::now());
            }
            if stop_asked.is_some_and(|asked| asked.elapsed() > STOP_WAIT) {
                return Outcome::Cancelled;
            }
            // What its person wrote meanwhile is its next message, and the answer stands there.
            // After a stop nothing more is handed to it: what was written then is new work.
            let came = if stop_asked.is_none() {
                going.take()
            } else {
                Vec::new()
            };
            for next in came {
                reply.went_on().await;
                *turn = next;
                *reply = scope.reply(turn);
                reply.taken().await;
                match tools_of(scope, reply) {
                    Ok(tools) => *work.heard.tools.lock_recover() = Some(tools),
                    Err(why) => return Outcome::Failed(why),
                }
                let (words, kind) = self.asked(&workspace, turn).await;
                let typed = is_their_yes(turn, kind.as_deref());
                if let Err(outcome) = self
                    .tell(work, (&workspace, &turn.agent), &words, typed)
                    .await
                {
                    return outcome;
                }
            }
            let Some(said) = work.heard.next(TICK).await else {
                if !work.coders.lives(&workspace, &turn.agent.id.0) && work.heard.quiet() {
                    // Its end is said a moment after it is over: one more wait hears it.
                    if gone {
                        return Outcome::Failed(GONE.to_owned());
                    }
                    gone = true;
                }
                continue;
            };
            if let Some(outcome) = self.take(work, &workspace, (turn, reply), said).await {
                return outcome;
            }
        }
    }

    /// `turn` was stopped: its harness is asked to end. A stop on its item says there who gave
    /// it; one on its message, through Chat, is its person's.
    async fn stop(&self, work: &Work, workspace: &str, turn: &Turn) {
        let on_item = self
            .read::<WorkspaceAgents>(workspace, CHAT, &agents_document())
            .await
            .ok()
            .flatten()
            .and_then(|known| {
                known
                    .get(&Id::from_raw(turn.agent.id.0.clone()))
                    .and_then(|agent| agent.sub.as_ref().map(|sub| sub.stands))
            })
            == Some(Stands::Stopped);
        if !on_item {
            *work.heard.stopped_by.lock_recover() = format!("person:{}", turn.agent.keeper);
        }
        work.coders.kept.stop(workspace, &turn.agent.id.0);
    }

    /// What `turn` asks in the message's own words, and the kind of the cause it carries. Its
    /// job and a note are their own words, a note with whose it is; anything else is what the
    /// message asks as every agent is told it.
    async fn asked(&self, workspace: &str, turn: &Turn) -> (String, Option<String>) {
        let Ask::Message { part, message, .. } = &turn.ask else {
            return (turn.request.clone(), None);
        };
        let Ok(Some(held)) = self
            .read::<chat::parts::ChatMessages>(workspace, CHAT, part)
            .await
        else {
            return (turn.request.clone(), None);
        };
        let Some(found) = held.messages.get(&Id::from_raw(message.clone())) else {
            return (turn.request.clone(), None);
        };
        let own = found.text.trim();
        match &found.cause {
            Some(cause) if cause.kind.0 == sub_agents::NOTE => (
                format!("A note from {}: {own}", cause.label),
                Some(cause.kind.0.clone()),
            ),
            Some(cause) if cause.kind.0 == sub_agents::JOB => {
                (own.to_owned(), Some(cause.kind.0.clone()))
            }
            Some(cause) => (chat::summons::request(found).0, Some(cause.kind.0.clone())),
            None => (chat::summons::request(found).0, None),
        }
    }

    /// Take one thing `work`'s harness said: the turn's outcome when that ends the turn.
    async fn take(
        &self,
        work: &mut Work,
        workspace: &str,
        (turn, reply): (&Turn, &Reply),
        said: Said,
    ) -> Option<Outcome> {
        match said {
            Said::Session(id) => {
                self.coder_says(workspace, &turn.agent, |report| report.session = Some(id))
                    .await;
                None
            }
            Said::Doing(what) => {
                work.show(reply).await;
                reply.doing(&what).await;
                None
            }
            Said::Says(words) => {
                work.show(reply).await;
                work.said = Some(words);
                None
            }
            Said::Finished(ok, words) => {
                work.pending = work.pending.saturating_sub(1);
                // What it said last is its answer, unless it ended on other words.
                let answer = match work.said.take() {
                    Some(last) if words.trim().is_empty() => last,
                    Some(last) if last.trim() != words.trim() => {
                        work.write(reply, &last).await;
                        words
                    }
                    _ => words,
                };
                if work.pending > 0 {
                    work.write(reply, &answer).await;
                    return None;
                }
                let answer = work.whole(answer);
                Some(if ok {
                    Outcome::Answered(answer)
                } else {
                    Outcome::Failed(answer)
                })
            }
            Said::Over(ended) => {
                self.coder_ended(workspace, (&turn.agent, &work.payer), &work.heard, &ended)
                    .await;
                Some(match ended.how {
                    Ending::Stopped => Outcome::Cancelled,
                    Ending::Finished => {
                        let last = work.said.take().unwrap_or_default();
                        Outcome::Answered(work.whole(last))
                    }
                    Ending::Failed | Ending::Lost => Outcome::Failed(ended.words),
                })
            }
        }
    }

    /// Count what `work`'s process spent since it was last counted on its payer's day, and say
    /// on the agent's item what it spent in all.
    async fn count(&self, workspace: &str, agent: &Agent, work: &Work) {
        if let Some(spent) = work.coders.kept.spent_micros(workspace, &agent.id.0) {
            self.counted(workspace, (agent, &work.payer), &work.heard, spent)
                .await;
        }
    }

    /// `agent`'s process has spent `spent` in all: what of it is not counted yet is counted on
    /// `payer`'s day, and the agent's item says what it spent over all its processes.
    async fn counted(
        &self,
        workspace: &str,
        (agent, payer): (&Agent, &Agent),
        heard: &Heard,
        spent: u64,
    ) {
        let before = heard.counted.fetch_max(spent, Ordering::SeqCst);
        let more = spent.saturating_sub(before);
        if more == 0 {
            return;
        }
        self.charge(workspace, payer, &today(), more).await;
        let all = heard
            .spent_before
            .load(Ordering::SeqCst)
            .saturating_add(spent);
        heard.all.fetch_max(all, Ordering::SeqCst);
        self.coder_says(workspace, agent, |report| {
            report.spent_micros = Some(all);
        })
        .await;
    }

    /// `agent`'s process is over as `ended`: its item says when, how and why, and who stopped
    /// it where Chat's stop did.
    async fn coder_ended(
        &self,
        workspace: &str,
        (agent, payer): (&Agent, &Agent),
        heard: &Heard,
        ended: &Ended,
    ) {
        self.counted(workspace, (agent, payer), heard, ended.spent_micros)
            .await;
        let process = CodingProcess {
            started_ms: heard.started_ms.load(Ordering::SeqCst),
            ended_ms: Some(tilo_clock::unix_now_ms_i64()),
            ended: ended.how.word().to_owned(),
            exit: ended.exit,
            words: ended.words.clone(),
            stopped_by: heard.stopped_by.lock_recover().clone(),
            command: String::new(),
        };
        self.coder_says(workspace, agent, |report| {
            report.process = Some(process);
        })
        .await;
    }

    /// Settle what `agent`'s harness said while no turn took it: only its end counts.
    async fn settle(&self, workspace: &str, (agent, payer): (&Agent, &Agent), heard: &Heard) {
        while let Some(said) = heard.pop() {
            if let Said::Over(ended) = said {
                self.coder_ended(workspace, (agent, payer), heard, &ended)
                    .await;
            }
        }
    }

    /// Say on its item how `agent`'s work in the machine stands, with what `named` names. The
    /// server that works it writes the item as the sub-agent itself, for the person who keeps
    /// it, as it writes how a sub-agent stands: no tool of a turn is asked for it.
    async fn coder_says(
        &self,
        workspace: &str,
        agent: &Agent,
        named: impl FnOnce(&mut verbs::SubAgentCoding),
    ) {
        let mut report = verbs::SubAgentCoding {
            agent: Id::from_raw(agent.id.0.clone()),
            process: None,
            session: None,
            spent_micros: None,
            harness: None,
        };
        named(&mut report);
        let (id, keeper) = (agent.id.0.clone(), agent.keeper.clone());
        let written = self
            .workspaces
            .author_application(
                workspace,
                self.workspaces.governor(),
                &tilo_agent_session::AppCatalogue::native(),
                CHAT,
                &agents_document(),
                |doc| {
                    let mut saying =
                        tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
                            .as_agent_for(&id, &keeper)
                            .in_app(CHAT, "Chat");
                    // A replica stamps what it writes with a fixed day of its own unless told the time.
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
            tracing::warn!(%workspace, agent = %agent.id.0, %error, "what a coding agent's item should say was not written");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_harness_with_no_turn_going_is_refused_its_requests_and_answered_no_note() {
        let refused = no_turn(&json!({"jsonrpc": "2.0", "id": 7, "method": "tools/list"}));
        assert_eq!(
            refused,
            Some(json!({
                "jsonrpc": "2.0",
                "id": 7,
                "error": {"code": -32000, "message": NO_TURN},
            }))
        );
        assert_eq!(
            no_turn(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})),
            None,
            "a message that asks for no answer gets none"
        );
    }

    #[test]
    fn only_the_keepers_own_words_are_a_yes_to_a_start() {
        let (ada, bob) = (hex::encode([8_u8; 32]), hex::encode([9_u8; 32]));
        assert!(is_keeper(&ada, &ada), "its keeper asks");
        assert!(
            !is_keeper(&ada, &bob),
            "another member of the workspace is not its keeper"
        );
        assert!(!is_keeper(&ada, ""), "an ask that names nobody");
        assert!(!is_keeper("", &ada), "an agent nobody keeps");
        assert!(!is_keeper("nobody", "nobody"), "names that are no person's");
    }
}
