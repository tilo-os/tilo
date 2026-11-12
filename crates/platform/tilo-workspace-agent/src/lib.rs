//! A workspace's agents: who works in it, each as one item of the workspace's
//! own record of agents, kept by Chat.
//!
//! An agent is not a member: members stay people. An agent is a record every
//! app of the workspace opens, so its name, its colour and the model it plans
//! with read the same in the settings that list it, in the chat it answers in,
//! and in the presence its work leaves. Its actor is `agent:<item id>`.
//!
//! The record is Chat's ([`OWNER`]): its actions are decided against Chat's
//! committed surface, by the workspace's grants, wherever they are invoked. An
//! editor names, bounds and retires agents; the person who keeps an agent — the
//! one whose devices run it and whose model credentials pay — alone chooses the
//! model it plans with.
//!
//! [`Agent::bound`] is a narrowing of what its keeper may do, never a widening:
//! an empty one follows the default (read and make undoable changes; outside
//! effects ask first). A rule that would let an outside effect run unasked is
//! refused, so no stored bound can make an agent reach further than the person
//! it works for.

use std::fmt;

use collab_types::presence::{PRESENCE_LEN, PresenceHue};
use serde::{Deserialize, Serialize};
use tilo_resource::items::item_resource_id;
use tilo_resource::search_index::{IndexKind, IndexPerson};
use tilo_resource::{
    ActionCx, Atom, Id, Identified, JsonSchema, Keyed, Model, NonBlank, Refusal, SearchEntry,
    resource,
};

#[cfg(test)]
mod tests;

/// The app whose record this is, and whose surface decides its changes:
/// Chat ships wherever a workspace is used, natively and in the browser.
pub const OWNER: &str = "so.tilo.chat";

/// The record's resource kind.
pub const KIND: &str = "chat.agent";

/// The longest name an agent is given.
pub const MAX_NAME: usize = 64;

/// The most agents a workspace keeps.
pub const MAX_AGENTS: usize = 250;

/// The longest a keeper's id may be.
const MAX_KEEPER: usize = 4096;

/// The longest a model's key (`provider/model`) may be.
const MAX_MODEL: usize = 200;

/// The most rules one bound holds.
pub const MAX_BOUND: usize = 64;

/// The most a keeper may let an agent spend in a day, in millionths of the
/// currency: ten thousand, far past what a day's work costs.
pub const MAX_SPEND_LIMIT_MICROS: u64 = 10_000_000_000;

/// What an agent spent on one day, as its keeper counted it.
///
/// The counter is kept per day, so a day that ends starts it again: a later
/// day's first charge replaces it, and no reading of an old day is a reading
/// of this one ([`Agent::spent_on`]).
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Spent {
    /// The day it counts, `YYYY-MM-DD` by its keeper's clock.
    pub day: String,
    /// What it spent that day, in millionths of the currency.
    pub cost_micros: u64,
}

/// One rule of an agent's bound, as the record stores it: the node, the
/// action kinds and the answer, each in the text form
/// [`tilo_action_policy::Rule`] is stored and sent in.
///
/// The policy's own [`Rule`](tilo_action_policy::Rule) is serde only, which a
/// record field and an action's argument cannot hold; this is the one place
/// its three parts cross between the two.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Bound {
    /// The node: `room:<workspace>/app:<app>/kind:<kind>/…`, `*` for an open
    /// level and `**` for any depth.
    pub node: String,
    /// The action kinds: `read`, `undoable`, `outside`.
    pub kinds: Vec<String>,
    /// `allow`, `ask` or `deny`.
    pub answer: String,
}

impl Bound {
    /// `rule` as the record stores it.
    #[must_use]
    pub fn of(rule: &tilo_action_policy::Rule) -> Self {
        Self {
            node: rule.node.to_string(),
            kinds: rule.kinds.iter().map(|kind| kind.to_string()).collect(),
            answer: rule.answer.to_string(),
        }
    }

    /// The rule this stores.
    pub fn rule(&self) -> Result<tilo_action_policy::Rule, Refusal> {
        use std::str::FromStr as _;
        let node = tilo_action_policy::TreePath::from_str(&self.node)
            .map_err(|error| Refusal::new(format!("{error}")))?;
        let mut kinds = tilo_action_policy::KindSet::NONE;
        for key in &self.kinds {
            let kind = match key.as_str() {
                "read" => tilo_action_policy::ActionKind::Read,
                "undoable" => tilo_action_policy::ActionKind::Undoable,
                "outside" => tilo_action_policy::ActionKind::Outside,
                other => {
                    return Err(Refusal::new(format!(
                        "{other:?} is not an action kind: read, undoable or outside"
                    )));
                }
            };
            kinds = kinds.with(kind);
        }
        if kinds.is_empty() {
            return Err(Refusal::new("a rule names at least one action kind"));
        }
        let answer = tilo_action_policy::Answer::parse(&self.answer)
            .ok_or_else(|| Refusal::new("an answer is allow, ask or deny"))?;
        Ok(tilo_action_policy::Rule::new(node, kinds, answer))
    }

    /// Whether this narrows rather than widens: an outside effect never runs
    /// unasked, whatever the person granting it may do.
    fn narrows(&self) -> bool {
        !(self.answer == "allow" && self.kinds.iter().any(|kind| kind == "outside"))
    }
}

/// Whether an agent is up for work or was retired: retiring keeps its name and
/// its conversations readable, and takes it out of the roster of who works here.
#[derive(Model, Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Works here.
    #[default]
    Active,
    /// Was told to stop: it takes no new work.
    Retired,
}

/// One agent of the workspace.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Agent {
    /// Its item id, and the id its actor carries: `agent:<id>`.
    pub id: Atom,
    /// Its name, as people read it and search finds it.
    #[model(label)]
    pub name: String,
    /// Its presence colour as its keeper chose it: a slot of the palette
    /// people are drawn in ([`PresenceHue`]). `None` follows its identity.
    pub colour: Option<u8>,
    /// Who keeps it: the member whose devices run it and whose model
    /// credentials pay. Only they choose its model.
    pub keeper: String,
    /// The model it plans with as `provider/model`; `None` follows the
    /// keeper's default.
    pub model: Option<String>,
    /// What it may do, narrowed from its keeper's grants; empty follows the
    /// default.
    pub bound: Vec<Bound>,
    /// The most its keeper lets it spend in a day, in millionths of the
    /// currency; `None` spends unmetered.
    pub spend_limit_micros: Option<u64>,
    /// What it spent on the last day its keeper counted.
    pub spent: Option<Spent>,
    /// Whether it works here.
    pub state: AgentState,
    /// When it was made, in milliseconds since the Unix epoch.
    pub created_at: i64,
}

impl Identified for Agent {
    fn id(&self) -> Id {
        Id::from_raw(self.id.0.clone())
    }
}

impl Agent {
    /// The identity its colour and its actor are derived from: `agent:<id>`.
    #[must_use]
    pub fn identity(&self) -> String {
        format!("agent:{}", self.id.0)
    }

    /// Its presence colour's slot: the one its keeper chose, or the one its
    /// identity gives it.
    #[must_use]
    pub fn slot(&self) -> u8 {
        self.colour.unwrap_or_else(|| {
            u8::try_from(PresenceHue::of_identity(&self.identity()).slot()).unwrap_or(0)
        })
    }

    /// Its presence colour, as people's colours are chosen.
    #[must_use]
    pub fn hue(&self) -> PresenceHue {
        PresenceHue::from_slot(usize::from(self.slot()))
    }

    /// The colour it is drawn in, as sRGB.
    #[must_use]
    pub fn rgb(&self) -> (u8, u8, u8) {
        collab_types::presence::identity_rgb(self.hue())
    }

    /// Whether it still works here.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.state == AgentState::Active
    }

    /// What it spent on `day`, in millionths of the currency: nothing on a
    /// day its keeper has not charged, so a new day starts it at zero.
    #[must_use]
    pub fn spent_on(&self, day: &str) -> u64 {
        self.spent
            .as_ref()
            .filter(|spent| spent.day == day)
            .map_or(0, |spent| spent.cost_micros)
    }

    /// Whether it may still spend on `day`: its keeper's limit is not
    /// reached yet, or there is none.
    #[must_use]
    pub fn within_budget(&self, day: &str) -> bool {
        self.spend_limit_micros
            .is_none_or(|limit| self.spent_on(day) < limit)
    }

    /// What it may do, as the policy reads it: the stored narrowing, or
    /// `None` following the default for its workspace.
    ///
    /// A stored rule the policy does not read cannot be here: the record's
    /// own check refuses one before any state holds it.
    #[must_use]
    pub fn bound_rules(&self) -> Option<Vec<tilo_action_policy::Rule>> {
        if self.bound.is_empty() {
            return None;
        }
        Some(
            self.bound
                .iter()
                .filter_map(|bound| bound.rule().ok())
                .collect(),
        )
    }
}

/// The workspace's agents, by id.
#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = WorkspaceAgents::valid)]
pub struct WorkspaceAgents {
    pub agents: Keyed<Agent>,
}

impl WorkspaceAgents {
    /// What search finds in the workspace's agents: each by the name people
    /// know it by, opening it at its item of the record.
    #[must_use]
    pub fn search(&self) -> Vec<SearchEntry> {
        self.agents
            .iter()
            .filter_map(|agent| {
                let name = agent.name.trim();
                (!name.is_empty()).then(|| {
                    let anchor = item_resource_id::<Self>("agents", &Id::from(agent.id.0.as_str()));
                    SearchEntry::new(name, "")
                        .anchored(anchor.clone())
                        .of_kind(IndexKind::Agent)
                        .showing(vec![IndexPerson {
                            name: name.to_owned(),
                            anchor,
                            identity: agent.identity(),
                            agent: true,
                        }])
                        .kept_by(IndexPerson {
                            name: String::new(),
                            anchor: String::new(),
                            identity: agent.keeper.clone(),
                            agent: false,
                        })
                })
            })
            .collect()
    }
}

impl WorkspaceAgents {
    /// What the record holds: the workspace's agents: each one's name, the member who keeps it,
    /// its model and its colour.
    ///
    /// It answers through [`fields`](tilo_resource::read::fields): a caller
    /// with no window open reads the record as it reads any app's record, and
    /// a named item is read from the record's table or by the read its own
    /// type gives it.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "chat.agent",
    label = "Agents",
    search = WorkspaceAgents::search,
    read_content = WorkspaceAgents::read_document
)]
impl WorkspaceAgents {
    /// Make an agent.
    ///
    /// A name people read (at most 64 characters), the member who keeps it
    /// (whoever asks here when none is named) and optionally a model as
    /// `provider/model` and the colour it is drawn in. Its colour follows its
    /// id, as a person's follows theirs, unless one is named. Its id is the
    /// caller's (`claude-code` for a client that named itself, an agent
    /// admitted before records were) or a fresh one.
    #[action]
    pub fn create_agent(
        &mut self,
        cx: &mut ActionCx,
        #[param("Its name, as people read it")] name: NonBlank,
        #[param("Who keeps it: a member's id; none for whoever makes it")] keeper: Option<String>,
        #[param("Its id: one already in use elsewhere; none to mint one")] id: Option<String>,
        #[param("The model it plans with, as provider/model; none for the keeper's default")]
        model: Option<String>,
        #[param("Its colour: a slot of the presence palette; none for one derived from its id")]
        colour: Option<u8>,
    ) -> Result<(), Refusal> {
        if self.agents.len() >= MAX_AGENTS {
            return Err(Refusal::new(format!(
                "a workspace keeps at most {MAX_AGENTS} agents"
            )));
        }
        let name = clean_name(&name)?;
        let me = author(cx)?;
        let keeper = match keeper.as_deref().map(str::trim) {
            None | Some("") => me,
            Some(keeper) => person(keeper)?,
        };
        let id = match id.as_deref().map(str::trim) {
            None | Some("") => cx.id().to_string(),
            Some(id) => {
                let id = given_id(id)?;
                if self.agents.find(&Id::from_raw(id.clone())).is_some() {
                    return Err(Refusal::new(format!("there is already an agent {id:?}")));
                }
                id
            }
        };
        let model = chosen_model(model)?;
        let colour = colour_slot(colour)?;
        self.agents.push(Agent {
            id: Atom(id),
            name,
            colour,
            keeper,
            model,
            bound: Vec::new(),
            spend_limit_micros: None,
            spent: None,
            state: AgentState::Active,
            created_at: cx.now_ms(),
        });
        Ok(())
    }

    /// Set the most an agent may spend in a day.
    ///
    /// The keeper's to choose, as the model is: their credentials pay for it.
    /// A limit of zero stops it working until its keeper raises it again;
    /// none spends unmetered. The day is its keeper's clock's, so a limit
    /// counts from midnight where the keeper sits.
    #[action(item = agents)]
    pub fn set_agent_spend_limit(
        &mut self,
        cx: &mut ActionCx,
        #[param("The agent")] agent: Id,
        #[param(
            "The most it may spend in a day, in millionths of the currency; none for no limit"
        )]
        limit_micros: Option<u64>,
    ) -> Result<(), Refusal> {
        let me = author(cx)?;
        let found = self
            .agents
            .find_mut(&agent)
            .ok_or_else(|| no_such(&agent))?;
        if !same_person(&found.keeper, &me) {
            return Err(Refusal::new(
                "only the agent's keeper sets the most it may spend in a day",
            ));
        }
        found.spend_limit_micros = checked_limit(limit_micros)?;
        Ok(())
    }

    /// Charge what an agent spent to the day's counter.
    ///
    /// The keeper's to write, on the device that runs the agent, from what the
    /// model gateway charged for each call: an agent never counts its own
    /// spending. A later day than the one counted starts the counter again; an
    /// earlier one is already over and changes nothing.
    #[action(item = agents)]
    pub fn record_agent_spend(
        &mut self,
        cx: &mut ActionCx,
        #[param("The agent")] agent: Id,
        #[param("The day counted, YYYY-MM-DD by its keeper's clock")] day: String,
        #[param("What it spent that day, in millionths of the currency")] cost_micros: u64,
    ) -> Result<(), Refusal> {
        let me = author(cx)?;
        let day = checked_day(&day)?;
        let found = self
            .agents
            .find_mut(&agent)
            .ok_or_else(|| no_such(&agent))?;
        if !same_person(&found.keeper, &me) {
            return Err(Refusal::new(
                "only the agent's keeper counts what it spends",
            ));
        }
        let counted = match &found.spent {
            Some(spent) if spent.day == day => Some(Spent {
                day,
                cost_micros: spent.cost_micros.saturating_add(cost_micros),
            }),
            Some(spent) if spent.day.as_str() > day.as_str() => return Ok(()),
            _ => Some(Spent { day, cost_micros }),
        };
        found.spent = counted;
        Ok(())
    }

    /// Set the colour an agent is drawn in.
    ///
    /// The keeper's to choose, as the model is: it is part of the agent's
    /// identity, the one its face and presence are drawn in everywhere.
    #[action(item = agents)]
    pub fn set_agent_colour(
        &mut self,
        cx: &mut ActionCx,
        #[param("The agent")] agent: Id,
        #[param("Its colour: a slot of the presence palette")] colour: u8,
    ) -> Result<(), Refusal> {
        let me = author(cx)?;
        let found = self
            .agents
            .find_mut(&agent)
            .ok_or_else(|| no_such(&agent))?;
        if !same_person(&found.keeper, &me) {
            return Err(Refusal::new(
                "only the agent's keeper chooses the colour it is drawn in",
            ));
        }
        found.colour = colour_slot(Some(colour))?;
        Ok(())
    }

    /// Name an agent.
    #[action(item = agents)]
    pub fn rename_agent(
        &mut self,
        #[param("The agent")] agent: Id,
        #[param("Its new name, as people read it")] name: NonBlank,
    ) -> Result<(), Refusal> {
        let name = clean_name(&name)?;
        let found = self
            .agents
            .find_mut(&agent)
            .ok_or_else(|| no_such(&agent))?;
        found.name = name;
        Ok(())
    }

    /// Choose the model an agent plans with.
    ///
    /// The keeper's to choose, alone: their devices run the agent and their
    /// credentials pay for it.
    #[action(item = agents)]
    pub fn set_agent_model(
        &mut self,
        cx: &mut ActionCx,
        #[param("The agent")] agent: Id,
        #[param("The model, as provider/model; none to follow the keeper's default")] model: Option<
            String,
        >,
    ) -> Result<(), Refusal> {
        let me = author(cx)?;
        let found = self
            .agents
            .find_mut(&agent)
            .ok_or_else(|| no_such(&agent))?;
        if !same_person(&found.keeper, &me) {
            return Err(Refusal::new(
                "only the agent's keeper chooses the model it plans with",
            ));
        }
        found.model = chosen_model(model)?;
        Ok(())
    }

    /// Bound what an agent may do.
    ///
    /// The rules narrow what its keeper's grants allow; none follows the
    /// default. An outside effect is never allowed unasked.
    #[action(item = agents)]
    pub fn set_agent_bound(
        &mut self,
        #[param("The agent")] agent: Id,
        #[param("The rules: node, kinds and answer; none for the default")] bound: Vec<Bound>,
    ) -> Result<(), Refusal> {
        if bound.len() > MAX_BOUND {
            return Err(Refusal::new(format!(
                "a bound holds at most {MAX_BOUND} rules"
            )));
        }
        for rule in &bound {
            rule.rule()?;
            if !rule.narrows() {
                return Err(Refusal::new(
                    "an agent never runs an outside effect unasked",
                ));
            }
        }
        let found = self
            .agents
            .find_mut(&agent)
            .ok_or_else(|| no_such(&agent))?;
        found.bound = bound;
        Ok(())
    }

    /// Retire an agent: it takes no new work.
    ///
    /// Its name and its conversations stay readable; nothing it did is lost.
    #[action(item = agents)]
    pub fn retire_agent(&mut self, #[param("The agent")] agent: Id) -> Result<(), Refusal> {
        let found = self
            .agents
            .find_mut(&agent)
            .ok_or_else(|| no_such(&agent))?;
        if found.state == AgentState::Retired {
            return Err(Refusal::new("that agent is retired already"));
        }
        found.state = AgentState::Retired;
        Ok(())
    }
}

impl WorkspaceAgents {
    fn valid(&self) -> Result<(), Refusal> {
        if self.agents.len() > MAX_AGENTS {
            return Err(Refusal::new(format!(
                "a workspace keeps at most {MAX_AGENTS} agents"
            )));
        }
        for agent in &self.agents {
            check_id(&agent.id.0)?;
            check_name(&agent.name)?;
            if agent
                .colour
                .is_some_and(|colour| usize::from(colour) >= PRESENCE_LEN)
            {
                return Err(Refusal::new(format!(
                    "{:?} wears a colour the palette does not have",
                    agent.name
                )));
            }
            person(&agent.keeper)?;
            if let Some(model) = &agent.model {
                check_model(model)?;
            }
            checked_limit(agent.spend_limit_micros)?;
            if let Some(spent) = &agent.spent {
                checked_day(&spent.day)?;
            }
            if agent.bound.len() > MAX_BOUND {
                return Err(Refusal::new(format!(
                    "a bound holds at most {MAX_BOUND} rules"
                )));
            }
            for rule in &agent.bound {
                rule.rule()?;
                if !rule.narrows() {
                    return Err(Refusal::new(
                        "an agent never runs an outside effect unasked",
                    ));
                }
            }
        }
        Ok(())
    }

    /// The agent `id`, whether it works here or was retired.
    #[must_use]
    pub fn get(&self, id: &Id) -> Option<&Agent> {
        self.agents.find(id)
    }

    /// The agent called `name`, the first one made.
    #[must_use]
    pub fn by_name(&self, name: &str) -> Option<&Agent> {
        self.agents.iter().find(|agent| agent.name == name)
    }

    /// The agents that work here, by id.
    pub fn active(&self) -> impl Iterator<Item = &Agent> {
        self.agents.iter().filter(|agent| agent.is_active())
    }

    /// An id for a new agent called `name` that no agent here holds: its
    /// name in lowercase letters, digits and dashes, `2`, `3`, … appended
    /// where that is taken. The actor it works as reads `agent:<id>`, so the
    /// id is made once and kept.
    #[must_use]
    pub fn free_id(&self, name: &str) -> String {
        let mut stem: String = name
            .trim()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        while stem.contains("--") {
            stem = stem.replace("--", "-");
        }
        let stem = stem.trim_matches('-').to_owned();
        let mut stem: String = stem.chars().take(MAX_NAME).collect();
        if stem.is_empty() {
            "agent".clone_into(&mut stem);
        }
        let taken = |id: &str| self.agents.iter().any(|agent| agent.id.0 == id);
        if !taken(&stem) {
            return stem;
        }
        (2..=MAX_AGENTS + 1)
            .map(|n| format!("{stem}-{n}"))
            .find(|id| !taken(id))
            .unwrap_or_else(|| format!("{stem}-{}", MAX_AGENTS + 2))
    }

    /// What each agent working here is called, by its bare id (its actor is `agent:<id>`): the
    /// one answer every app names an agent by.
    #[must_use]
    pub fn active_names(&self) -> std::collections::HashMap<String, String> {
        self.active()
            .filter(|agent| !agent.name.trim().is_empty())
            .map(|agent| (agent.id.0.clone(), agent.name.trim().to_owned()))
            .collect()
    }

    /// Every agent's name, as search names them.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.agents
            .iter()
            .map(|agent| agent.name.as_str())
            .collect()
    }
}

impl fmt::Display for AgentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Active => "active",
            Self::Retired => "retired",
        })
    }
}

fn no_such(agent: &Id) -> Refusal {
    Refusal::new(format!("there is no agent {agent}"))
}

/// Who the action is by: an agent is always someone's.
fn author(cx: &ActionCx) -> Result<String, Refusal> {
    cx.author()
        .filter(|author| !author.is_empty() && author.len() <= MAX_KEEPER)
        .map(str::to_owned)
        .ok_or_else(|| Refusal::new("an agent is always made by someone"))
}

/// Whether two stored ids name the same person: a member's root, with or
/// without the `person:` prefix grants carry.
#[must_use]
pub fn same_person(a: &str, b: &str) -> bool {
    root_of(a) == root_of(b)
}

fn root_of(id: &str) -> &str {
    id.strip_prefix("person:").unwrap_or(id)
}

fn person(id: &str) -> Result<String, Refusal> {
    let id = id.trim();
    if id.is_empty() || id.len() > MAX_KEEPER {
        return Err(Refusal::new("an agent is kept by a member"));
    }
    Ok(id.to_owned())
}

fn clean_name(name: &NonBlank) -> Result<String, Refusal> {
    let name = name.as_str().trim().to_owned();
    check_name(&name)?;
    Ok(name)
}

fn check_name(name: &str) -> Result<(), Refusal> {
    if name.trim().is_empty() || name.chars().count() > MAX_NAME {
        return Err(Refusal::new(format!(
            "an agent's name is 1 to {MAX_NAME} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(Refusal::new("an agent's name is one line of text"));
    }
    Ok(())
}

fn check_id(id: &str) -> Result<(), Refusal> {
    if id.trim().is_empty() || id.len() > MAX_KEEPER || id.chars().any(char::is_control) {
        return Err(Refusal::new("an agent's id is one short line of text"));
    }
    Ok(())
}

/// An id a caller names an agent by: one every host reads the same, so a
/// client that names itself is the same agent on every device.
fn given_id(id: &str) -> Result<String, Refusal> {
    let id = id.trim();
    check_id(id)?;
    Ok(id.to_owned())
}

/// A colour the presence palette has, or `None` following the agent's id.
fn colour_slot(colour: Option<u8>) -> Result<Option<u8>, Refusal> {
    let Some(colour) = colour else {
        return Ok(None);
    };
    if usize::from(colour) >= PRESENCE_LEN {
        return Err(Refusal::new(format!(
            "{colour} is not a colour the presence palette has"
        )));
    }
    Ok(Some(colour))
}

fn chosen_model(model: Option<String>) -> Result<Option<String>, Refusal> {
    let Some(model) = model.map(|model| model.trim().to_owned()) else {
        return Ok(None);
    };
    if model.is_empty() {
        return Ok(None);
    }
    check_model(&model)?;
    Ok(Some(model))
}

/// The most a keeper may set as a day's limit, or `None` for no limit.
fn checked_limit(limit: Option<u64>) -> Result<Option<u64>, Refusal> {
    match limit {
        Some(limit) if limit > MAX_SPEND_LIMIT_MICROS => Err(Refusal::new(format!(
            "a day's limit is at most {} dollars",
            MAX_SPEND_LIMIT_MICROS / 1_000_000
        ))),
        limit => Ok(limit),
    }
}

/// A day as the counter keeps it, `YYYY-MM-DD` by its keeper's clock.
fn checked_day(day: &str) -> Result<String, Refusal> {
    let day = day.trim();
    let digits = day.split('-').map(str::len).collect::<Vec<_>>();
    let shaped = digits == [4, 2, 2] && day.chars().all(|c| c.is_ascii_digit() || c == '-');
    if !shaped {
        return Err(Refusal::new(
            "a day is counted as YYYY-MM-DD, by the keeper's clock",
        ));
    }
    Ok(day.to_owned())
}

fn check_model(model: &str) -> Result<(), Refusal> {
    if model.len() > MAX_MODEL {
        return Err(Refusal::new("a model's key is at most 200 characters"));
    }
    let Some((profile, name)) = model.split_once('/') else {
        return Err(Refusal::new("a model is named as provider/model"));
    };
    if profile.is_empty() || name.is_empty() {
        return Err(Refusal::new("a model is named as provider/model"));
    }
    Ok(())
}
