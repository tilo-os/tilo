//! Sub-agents: the agents an agent starts for the person it works for.
//!
//! A person's main agent starts sub-agents: each gets one job, a role in words, a kind of work
//! that picks its model. Whoever started one reads how it stands and steers it while it works.
//! A sub-agent may start sub-agents of its own, as deep as its person allows, and the same holds
//! at every level: a lead with fifty workers is as ordinary as one helper.
//!
//! A sub-agent is an agent like any other: an item of the workspace's record of its agents
//! whose [`Agent::sub`] is set. Nothing keeps a second list of them.
//!
//! The record holds what is wanted of one, each a recorded change: that it was started
//! ([`WorkspaceAgents::start_sub_agent`]), a note its starter gives it
//! ([`WorkspaceAgents::note_sub_agent`]), that it is to stop
//! ([`WorkspaceAgents::stop_sub_agent`]). Whoever works it asks it each job and note as the agent
//! that gave it, answers those asks as the sub-agent, and writes back how it stands
//! ([`WorkspaceAgents::sub_agent_stands`]), so a runner that restarts finds everything it owes
//! in the records.
//!
//! **A sub-agent is one more actor in its starter's chain.** It works for the person its
//! starter's turn answered, and its work is decided for that person bounded by every agent and
//! person before it ([`tilo_action_policy::Acting::handed_on`]): it does no more than its
//! starter's turn may, and reads what that chain's grants allow. A sub-agent changes nothing of
//! this record but its own team ([`WorkspaceAgents::not_by_sub_agent`]), so it can neither
//! unbind its starter nor make an agent that nothing binds.

use std::fmt;

use serde::{Deserialize, Serialize};
use tilo_resource::{ActionCx, Atom, Id, JsonSchema, Model, Refusal};

use crate::{Agent, AgentState, WorkspaceAgents};

/// The longest job, in bytes: what a sub-agent is asked to do, its first ask. Longer material
/// belongs in a file or a note it is pointed at.
pub const MAX_JOB: usize = 4 * 1024;

/// The longest role, in bytes: a few words.
pub const MAX_ROLE: usize = 120;

/// The longest line of how one stands, in bytes.
pub const MAX_LINE: usize = 240;

/// The longest note, in bytes.
pub const MAX_NOTE: usize = 2 * 1024;

/// The notes a sub-agent's item keeps: the newest ones. Each is an ask of it for good once its
/// runner asked it.
pub const MAX_NOTES: usize = 8;

/// The longest address of the issue a sub-agent works for, in bytes.
pub const MAX_TASK: usize = 512;

/// The sub-agents a workspace keeps, beside the agents its people made: the oldest that ended
/// make room for a new one.
pub const MAX_SUB_AGENTS: usize = 400;

/// How deep sub-agents go under a person's agent that was told nothing else: it starts
/// sub-agents, and those start none.
pub const DEFAULT_DEPTH: u8 = 1;

/// The deepest a person may let them go.
pub const MAX_DEPTH: u8 = 5;

/// How many sub-agents work at once under a person's agent that was told nothing else.
pub const DEFAULT_AT_ONCE: u32 = 50;

/// The most a person may let work at once.
pub const MAX_AT_ONCE: u32 = 250;

/// The kinds of work a sub-agent's model is chosen by, as a person's model settings name them.
pub const KINDS: [&str; 5] = ["subtask", "task", "request", "classifier", "one_shot"];

/// The kind of work of a sub-agent that was given none: a part of a task.
pub const DEFAULT_KIND: &str = "subtask";

/// How a sub-agent stands.
#[derive(Model, Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Stands {
    /// It works on its job, or on what was written to it since.
    #[default]
    Working,
    /// It waits for its person: a question of its own waits for an answer, or it stopped at a
    /// limit only they can raise.
    NeedsPerson,
    /// Its work ended with an answer.
    Done,
    /// It could not work, or its work failed.
    Failed,
    /// It was stopped.
    Stopped,
}

impl Stands {
    /// Whether it holds one of the places its person lets work at once: it works, or waits for
    /// them to go on.
    #[must_use]
    pub const fn at_work(self) -> bool {
        matches!(self, Self::Working | Self::NeedsPerson)
    }
}

impl fmt::Display for Stands {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Working => "working",
            Self::NeedsPerson => "needs_person",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        })
    }
}

/// A note a sub-agent's starter gave it while it worked.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentNote {
    /// Its id among the notes of its sub-agent.
    pub id: String,
    /// The agent that gave it, by its id.
    pub by: String,
    /// What it says.
    pub text: String,
    /// When it was given, in milliseconds since the Unix epoch.
    pub at: i64,
}

/// What makes an agent a sub-agent: who started it, for what, and how it stands.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Sub {
    /// The agent that started it and steers it, by its id: the person's own agent, or a
    /// sub-agent for its own team.
    pub started_by: String,
    /// The job, in words: what it was started for, and what it is asked first.
    pub job: String,
    /// Its role in a few words, as "Researcher"; empty when it was given none.
    pub role: String,
    /// The kind of work that picks its model ([`KINDS`]).
    pub kind: String,
    /// How far below the person's own agent it stands: 1 when that agent started it.
    pub depth: u8,
    /// The issue in Tasks it works for, by its address; `None` when it works for none.
    pub task: Option<String>,
    /// How it stands.
    pub stands: Stands,
    /// When it came to stand so, in milliseconds since the Unix epoch.
    pub since: i64,
    /// One line of how it stands: empty while it works, else the start of its last answer or
    /// why it could not work.
    pub line: String,
    /// The newest notes its starter gave it ([`MAX_NOTES`]), oldest first.
    pub notes: Vec<AgentNote>,
    /// What it made, each a thing one can open ([`crate::Produced`]), oldest first.
    pub produced: Vec<crate::Produced>,
    /// Its work by a coding harness in a repository ([`crate::coding`]); `None` for a
    /// sub-agent that is Tilo's own loop.
    pub coding: Option<crate::Coding>,
}

/// A sub-agent to start: the parameters of [`WorkspaceAgents::start_sub_agent`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewSubAgent {
    /// What people call it: a few words naming its job
    pub name: String,
    /// The job, in words: the first thing it is told
    pub job: String,
    /// Its role in a few words, as Researcher
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// How it is to work, in words
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    /// The kind of work that picks its model: `subtask`, `task`, `request`, `classifier` or
    /// `one_shot`; `subtask` when left out
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The model it works with, as provider/model; leave it out for its person's choice for
    /// the kind
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The issue in Tasks it works for, by its address
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Its id: letters, digits and dashes; leave it out and its name makes one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Have a coding harness do the work on a repository: the folder it works on, and when
    /// not the default which harness, its branch and what the branch starts from
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coding: Option<crate::NewCoding>,
}

fn by_an_agent() -> Refusal {
    Refusal::new("a sub-agent is started and steered by an agent: ask your own agent for it")
}

fn not_a_sub_agent(agent: &Id) -> Refusal {
    Refusal::new(format!("{agent} is not a sub-agent"))
}

/// The agent invoking, and the person it works for: the one whose message its turn answers,
/// first in its chain. Whoever else the chain names (the person its agent belongs to, the agents
/// that started it) bounds what it does where every action is decided, not here.
fn agent_acting(cx: &ActionCx) -> Result<(String, String), Refusal> {
    let acting = cx.invoked_by().ok_or_else(by_an_agent)?;
    let agent = acting.agent_id().ok_or_else(by_an_agent)?.to_owned();
    let person = acting
        .acted_for_person()
        .filter(|person| !person.is_empty())
        .ok_or_else(|| {
            Refusal::new("an agent starts and steers sub-agents for a person it works for")
        })?;
    Ok((agent, person.to_owned()))
}

/// Refuses `text` that is no plain text of at most `most` bytes.
fn check_plain(text: &str, most: usize, what: &str) -> Result<(), Refusal> {
    if text.len() > most {
        return Err(Refusal::new(format!("{what} is at most {most} bytes")));
    }
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(Refusal::new(format!("{what} is plain text")));
    }
    Ok(())
}

/// `text` as it is kept: trimmed, plain, at most `most` bytes and saying something.
fn plain(text: &str, most: usize, what: &str) -> Result<String, Refusal> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Refusal::new(format!("{what} says something")));
    }
    check_plain(text, most, what)?;
    Ok(text.to_owned())
}

/// Refuses `text` that is more than one line of at most `most` bytes.
pub(crate) fn check_words(text: &str, most: usize, what: &str) -> Result<(), Refusal> {
    if text.len() > most || text.chars().any(char::is_control) {
        return Err(Refusal::new(format!(
            "{what} is a few words on one line, at most {most} bytes"
        )));
    }
    Ok(())
}

/// What was given for an optional line of words: trimmed, `None` when nothing was.
pub(crate) fn words(
    given: Option<String>,
    most: usize,
    what: &str,
) -> Result<Option<String>, Refusal> {
    let Some(given) = given.map(|given| given.trim().to_owned()) else {
        return Ok(None);
    };
    if given.is_empty() {
        return Ok(None);
    }
    check_words(&given, most, what)?;
    Ok(Some(given))
}

/// The first line of `text` that says something, cut to `most` bytes at a character.
pub(crate) fn one_line(text: &str, most: usize) -> String {
    let line: String = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let mut end = line.len().min(most);
    while !line.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    line.get(..end).unwrap_or_default().to_owned()
}

/// Refuses an agent whose record of being a sub-agent is not one this crate wrote.
pub(crate) fn check(agent: &Agent) -> Result<(), Refusal> {
    if agent.sub_depth.is_some_and(|depth| depth > MAX_DEPTH)
        || agent.sub_at_once.is_some_and(|most| most > MAX_AT_ONCE)
    {
        return Err(Refusal::new(format!(
            "sub-agents go at most {MAX_DEPTH} deep and {MAX_AT_ONCE} work at once"
        )));
    }
    let Some(sub) = &agent.sub else {
        return Ok(());
    };
    crate::check_id(&sub.started_by)?;
    check_plain(&sub.job, MAX_JOB, "a sub-agent's job")?;
    check_words(&sub.role, MAX_ROLE, "a role")?;
    check_words(&sub.line, MAX_LINE, "how a sub-agent stands")?;
    if let Some(task) = &sub.task {
        check_words(task, MAX_TASK, "the issue a sub-agent works for")?;
    }
    if !KINDS.contains(&sub.kind.as_str()) {
        return Err(Refusal::new(format!(
            "a sub-agent's kind of work is one of: {}",
            KINDS.join(", ")
        )));
    }
    if sub.depth == 0 || sub.depth > MAX_DEPTH {
        return Err(Refusal::new(format!(
            "a sub-agent stands 1 to {MAX_DEPTH} below its person's agent"
        )));
    }
    if sub.notes.len() > MAX_NOTES {
        return Err(Refusal::new(format!(
            "a sub-agent keeps its newest {MAX_NOTES} notes"
        )));
    }
    for note in &sub.notes {
        check_plain(&note.text, MAX_NOTE, "a note")?;
    }
    crate::produced::check(sub)?;
    crate::coding::check(sub)
}

impl WorkspaceAgents {
    /// The sub-agents started for `person` that work here, newest first.
    pub fn sub_agents_of<'a>(&'a self, person: &'a str) -> impl Iterator<Item = &'a Agent> {
        let mut theirs: Vec<&Agent> = self
            .agents
            .iter()
            .filter(|agent| agent.is_active() && agent.sub.is_some() && agent.acts_for == person)
            .collect();
        theirs.sort_by_key(|agent| std::cmp::Reverse(agent.created_at));
        theirs.into_iter()
    }

    /// The agent `agent`'s line of starters ends at: the person's own agent that started the
    /// first of them, or `agent` itself when nothing started it. Its day and its limits are the
    /// ones every sub-agent under it works within.
    #[must_use]
    pub fn root_of<'a>(&'a self, agent: &'a Agent) -> &'a Agent {
        let mut at = agent;
        for _ in 0..=MAX_DEPTH {
            let Some(above) = at
                .sub
                .as_ref()
                .and_then(|sub| self.agents.find(&Id::from_raw(sub.started_by.clone())))
            else {
                break;
            };
            at = above;
        }
        at
    }

    /// Whether the agent `above` is in `agent`'s line of starters: it started it, or started
    /// the one that did.
    #[must_use]
    pub fn started_under(&self, agent: &Agent, above: &str) -> bool {
        let mut at = agent;
        for _ in 0..=MAX_DEPTH {
            let Some(sub) = &at.sub else {
                return false;
            };
            if sub.started_by == above {
                return true;
            }
            let Some(next) = self.agents.find(&Id::from_raw(sub.started_by.clone())) else {
                return false;
            };
            at = next;
        }
        false
    }

    /// The team under `above`: the sub-agents it started and theirs that work here, oldest
    /// first. Nothing of whoever started `above` is among them, and nothing of a sub-agent
    /// beside it: whoever started sub-agents reads those below it and nobody else.
    #[must_use]
    pub fn team_of<'a>(&'a self, above: &Agent) -> Vec<&'a Agent> {
        let mut team: Vec<&Agent> = self
            .agents
            .iter()
            .filter(|agent| agent.is_active() && self.started_under(agent, &above.id.0))
            .collect();
        team.sort_by(|one, other| {
            (one.created_at, &one.id.0).cmp(&(other.created_at, &other.id.0))
        });
        team
    }

    /// Refuses an action a sub-agent invokes. A sub-agent changes nothing of the workspace's
    /// agents but its own team: it could otherwise lift what binds it, or make an agent that
    /// nothing binds.
    pub(crate) fn not_by_sub_agent(&self, cx: &ActionCx) -> Result<(), Refusal> {
        let Some(acting) = cx.invoked_by() else {
            return Ok(());
        };
        let by_one = acting
            .nodes()
            .into_iter()
            .filter_map(tilo_action_policy::Acting::agent_id)
            .any(|agent| {
                self.agents
                    .find(&Id::from_raw(agent.to_owned()))
                    .is_some_and(|agent| agent.sub.is_some())
            });
        if by_one {
            return Err(Refusal::new(
                "a sub-agent changes nothing of the workspace's agents but its own team",
            ));
        }
        Ok(())
    }

    /// The sub-agent `agent` that works here.
    fn working_sub(&self, agent: &Id) -> Result<&Agent, Refusal> {
        let found = self
            .agents
            .find(agent)
            .ok_or_else(|| crate::no_such(agent))?;
        if found.sub.is_none() {
            return Err(not_a_sub_agent(agent));
        }
        if !found.is_active() {
            return Err(Refusal::new("that sub-agent was dismissed"));
        }
        Ok(found)
    }

    /// Refuses unless the agent `by` steers the sub-agent `agent`: it started it, or one above
    /// it.
    fn steered_by(&self, agent: &Id, by: &str) -> Result<(), Refusal> {
        let found = self.working_sub(agent)?;
        if !self.started_under(found, by) {
            return Err(Refusal::new(
                "only the agent that started a sub-agent, or one above it, steers it",
            ));
        }
        Ok(())
    }

    /// The sub-agent `agent`'s own record of being one, when it is `agent` itself that
    /// invokes: what only its own runner writes is written
    /// through this, by every verb that is the runner's.
    ///
    /// # Errors
    /// Anyone else invokes, or `agent` is no sub-agent that works here.
    pub fn as_itself(&mut self, cx: &ActionCx, agent: &Id) -> Result<&mut Sub, Refusal> {
        let (by, _) = agent_acting(cx)?;
        if by != agent.as_str() {
            return Err(Refusal::new(
                "only a sub-agent's own runner says how it stands",
            ));
        }
        self.working_sub(agent)?;
        self.sub_mut(agent)
    }

    fn sub_mut(&mut self, agent: &Id) -> Result<&mut Sub, Refusal> {
        self.agents
            .find_mut(agent)
            .ok_or_else(|| crate::no_such(agent))?
            .sub
            .as_mut()
            .ok_or_else(|| not_a_sub_agent(agent))
    }

    /// Room for one more sub-agent: the oldest that ended and started nothing that is still
    /// kept leaves the record once it is full. Its asks and replies stay.
    pub(crate) fn make_room(&mut self) -> Result<(), Refusal> {
        let held = self
            .agents
            .iter()
            .filter(|agent| agent.sub.is_some())
            .count();
        if held < MAX_SUB_AGENTS {
            return Ok(());
        }
        let oldest = self
            .agents
            .iter()
            .filter(|agent| {
                agent
                    .sub
                    .as_ref()
                    .is_some_and(|sub| !agent.is_active() || !sub.stands.at_work())
                    && !self.agents.iter().any(|other| {
                        other
                            .sub
                            .as_ref()
                            .is_some_and(|sub| sub.started_by == agent.id.0)
                    })
            })
            .min_by_key(|agent| agent.created_at)
            .map(|agent| Id::from_raw(agent.id.0.clone()));
        match oldest {
            Some(oldest) => {
                self.agents.take(&oldest);
                Ok(())
            }
            None => Err(Refusal::new(format!(
                "a workspace keeps at most {MAX_SUB_AGENTS} sub-agents, and all of them are at work"
            ))),
        }
    }

    /// How far below its person's own agent a sub-agent that `by` starts now stands, where
    /// that agent's person allows one more: so deep, and so many at work at once.
    fn place_under(&self, by: &Agent) -> Result<u8, Refusal> {
        let root = self.root_of(by);
        let depth = by.sub.as_ref().map_or(0, |sub| sub.depth).saturating_add(1);
        let deepest = root.sub_depth.unwrap_or(DEFAULT_DEPTH);
        if depth > deepest {
            return Err(Refusal::new(format!(
                "sub-agents go {deepest} deep under {:?}: its person allows no more",
                root.name
            )));
        }
        let most = root.sub_at_once.unwrap_or(DEFAULT_AT_ONCE);
        let at_work = self
            .agents
            .iter()
            .filter(|agent| {
                agent.is_active()
                    && agent.sub.as_ref().is_some_and(|sub| sub.stands.at_work())
                    && self.root_of(agent).id == root.id
            })
            .count();
        if at_work >= usize::try_from(most).unwrap_or(usize::MAX) {
            return Err(Refusal::new(format!(
                "{most} sub-agents are at work under {:?} already: wait for one to end, stop \
                 one, or ask its person to allow more",
                root.name
            )));
        }
        Ok(depth)
    }

    pub(crate) fn start_sub(&mut self, cx: &ActionCx, new: NewSubAgent) -> Result<(), Refusal> {
        let (starter, person) = agent_acting(cx)?;
        let by = self
            .agents
            .find(&Id::from_raw(starter.clone()))
            .filter(|by| by.is_active())
            .ok_or_else(|| Refusal::new("only an agent that works here starts a sub-agent"))?;
        let depth = self.place_under(by)?;
        let name = new.name.trim().to_owned();
        crate::check_name(&name)?;
        let job = plain(&new.job, MAX_JOB, "a sub-agent's job")?;
        let role = words(new.role, MAX_ROLE, "a role")?.unwrap_or_default();
        let instruction = crate::told_instruction(new.instruction)?;
        let kind = match new.kind.as_deref().map(str::trim) {
            None | Some("") => DEFAULT_KIND.to_owned(),
            Some(kind) if KINDS.contains(&kind) => kind.to_owned(),
            Some(kind) => {
                return Err(Refusal::new(format!(
                    "{kind:?} is not a kind of work: one of {}",
                    KINDS.join(", ")
                )));
            }
        };
        let model = crate::chosen_model(new.model)?;
        let task = words(new.task, MAX_TASK, "the issue a sub-agent works for")?;
        let id = match new.id.as_deref().map(str::trim) {
            None | Some("") => self.free_id(&name),
            Some(id) => crate::given_id(id)?,
        };
        if self.agents.find(&Id::from_raw(id.clone())).is_some() {
            return Err(Refusal::new(format!("there is already an agent {id:?}")));
        }
        let coding = new
            .coding
            .map(|coding| crate::coding::started(coding, &id))
            .transpose()?;
        self.make_room()?;
        let now = cx.now_ms();
        self.agents.push(Agent {
            id: Atom(id),
            name,
            avatar: None,
            acts_for: person,
            model,
            mcp_client: None,
            spend_limit_micros: None,
            spent: None,
            state: AgentState::Active,
            created_at: now,
            instruction,
            sub: Some(Sub {
                started_by: starter,
                job,
                role,
                kind,
                depth,
                task,
                stands: Stands::Working,
                since: now,
                line: String::new(),
                notes: Vec::new(),
                produced: Vec::new(),
                coding,
            }),
            sub_depth: None,
            sub_at_once: None,
            runs_on: Vec::new(),
        });
        Ok(())
    }

    pub(crate) fn note_sub(
        &mut self,
        cx: &mut ActionCx,
        agent: &Id,
        text: &str,
    ) -> Result<(), Refusal> {
        let (by, _) = agent_acting(cx)?;
        let text = plain(text, MAX_NOTE, "a note")?;
        self.steered_by(agent, &by)?;
        let (id, at) = (cx.id().to_string(), cx.now_ms());
        let sub = self.sub_mut(agent)?;
        sub.notes.push(AgentNote { id, by, text, at });
        let over = sub.notes.len().saturating_sub(MAX_NOTES);
        sub.notes.drain(..over);
        Ok(())
    }

    pub(crate) fn stop_sub(&mut self, cx: &ActionCx, agent: &Id) -> Result<(), Refusal> {
        let acting = cx.invoked_by().ok_or_else(by_an_agent)?;
        let by = if acting.is_agent() {
            let (by, _) = agent_acting(cx)?;
            self.steered_by(agent, &by)?;
            format!("agent:{by}")
        } else {
            // A member themselves, and nothing that only acts for them.
            let person = acting.person_id().ok_or_else(|| {
                Refusal::new("a sub-agent is stopped by a member, or by the agent that steers it")
            })?;
            self.working_sub(agent)?;
            format!("person:{person}")
        };
        let now = cx.now_ms();
        let sub = self.sub_mut(agent)?;
        if sub.stands.at_work() {
            sub.stands = Stands::Stopped;
            sub.since = now;
            sub.line.clear();
            crate::coding::stopped(sub, by);
        }
        Ok(())
    }

    pub(crate) fn sub_stands(
        &mut self,
        cx: &ActionCx,
        agent: &Id,
        stands: Stands,
        line: Option<&str>,
    ) -> Result<(), Refusal> {
        let now = cx.now_ms();
        let line = one_line(line.unwrap_or_default(), MAX_LINE);
        let sub = self.as_itself(cx, agent)?;
        // A stop holds until new work begins: how the work it stopped ended does not undo it.
        if sub.stands == Stands::Stopped && stands != Stands::Working {
            return Ok(());
        }
        if sub.stands != stands {
            sub.stands = stands;
            sub.since = now;
        }
        if sub.line != line {
            sub.line = line;
        }
        Ok(())
    }

    pub(crate) fn set_sub_limits(
        &mut self,
        cx: &ActionCx,
        agent: &Id,
        depth: Option<u8>,
        at_once: Option<u32>,
    ) -> Result<(), Refusal> {
        crate::in_person(cx)?;
        if depth.is_some_and(|depth| depth > MAX_DEPTH) {
            return Err(Refusal::new(format!(
                "sub-agents go at most {MAX_DEPTH} deep"
            )));
        }
        if at_once.is_some_and(|most| most > MAX_AT_ONCE) {
            return Err(Refusal::new(format!(
                "at most {MAX_AT_ONCE} sub-agents work at once"
            )));
        }
        let found = self
            .agents
            .find_mut(agent)
            .ok_or_else(|| crate::no_such(agent))?;
        if found.sub.is_some() {
            return Err(Refusal::new(
                "the limits are set on the agent a person made, and hold for every sub-agent \
                 under it",
            ));
        }
        found.sub_depth = depth;
        found.sub_at_once = at_once;
        Ok(())
    }
}
