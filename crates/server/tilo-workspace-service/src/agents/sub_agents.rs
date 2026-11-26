//! Sub-agents on the server: what the workspace's list of its agents asks for one is carried
//! into its conversation, and how it stands is written back.
//!
//! A sub-agent is an agent like the others this server works ([`tilo_workspace_agent::sub`]).
//! What differs is here, and nowhere else in the runtime:
//!
//! - **Where it answers.** Only in its own conversation: a message that names it elsewhere is
//!   no turn of its, since a turn is told the conversation it is in.
//! - **What it is told.** Its job is the first message of its conversation, and each note its
//!   starter gives it a later one, written there by this server as the starter's own words
//!   (`carry`). The list holds both, so a server that restarts finds what it
//!   still owes. A note reaches a turn that is going at its next step, as the person's own
//!   words do.
//! - **Who pays.** Its model calls are counted on the day of the person's own agent its line
//!   of starters ends at, within that agent's limit, and are asked of the model its person
//!   chose for its kind of work ([`payer`], [`kind_of`]).
//! - **How it stands.** Written when a turn of it begins and when it ends, as the sub-agent
//!   itself. When work its starter gave it ends, its starter is told in the starter's own
//!   conversation (`sub_ended`).
//! - **Stopping.** A stop on its item ends the turn that is going, and every ask that stood
//!   before the stop ([`Team::settle`]).
//!
//! What a sub-agent may read and change is not decided here: it is one more actor in its
//! starter's chain ([`tilo_actions::remote::Caller::working_under`]), decided as every action is.

use std::collections::BTreeMap;

use chat::chat::{Chat, Message, reply_id_at};
use collaboration::permissions::MemberId;
use serde_json::{Map, json};
use tilo_actions::remote::Caller;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_resource::Id;
use tilo_workspace_agent::{Agent, Stands, WorkspaceAgents, sub};

use super::{
    Ask, Asks, CHAT, Outcome, ServerAgents, Turn, WorkspaceActionProvider, WorkspaceId, going,
    invoke_named,
};
use crate::workspaces::person_model::Interaction;

/// The kind of the cause a sub-agent's job is written into its conversation with.
pub(super) const JOB: &str = "job";
/// The kind of the cause a note of its starter's is written with.
pub(super) const NOTE: &str = "note";
/// The kind of the cause a person asks an agent they keep for a sub-agent with, through Chat's
/// `ask-agent`: the words of the ask are the job, the cause's label is the sub-agent's name.
const START: &str = "sub-agent";
/// The kind of the cause that wakes an agent with its person's answer to a question the agent
/// asked: the work it asked within goes on.
const ANSWER: &str = "answer";
/// Chat's name for the argument that says which conversation a message goes to.
const TO: &str = "channel";
/// The most of a sub-agent's answer its starter is told, in characters: the rest is in the
/// sub-agent's conversation.
const TOLD_CHARS: usize = 2_000;
/// The longest label a cause carries, in bytes (Chat's own limit).
const LABEL_BYTES: usize = 120;
/// The longest key a cause carries, in bytes (Chat's own limit).
const KEY_BYTES: usize = 200;

/// Where a sub-agent's turn stands in its team.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// The person's own agent its line of starters ends at: its model calls are counted on
    /// that agent's day.
    pub root: Agent,
    /// The agent that started it, while the list still holds that one.
    pub starter: Option<Agent>,
    /// Whether what the turn answers came from its line of starters (its job, a note) rather
    /// than from its person: when such work ends, its starter is told.
    pub from_starter: bool,
}

/// One message this server owes a sub-agent's conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Delivery {
    /// The person the sub-agent works for.
    person: String,
    /// The agent whose words it is: the sub-agent's starter for its job, whoever gave a note.
    by: String,
    by_name: String,
    /// The sub-agent's conversation.
    conversation: String,
    kind: &'static str,
    key: String,
    text: String,
}

/// What a look found this server owes a workspace's sub-agents beside their turns.
#[derive(Default)]
pub(super) struct Owed {
    /// Jobs and notes the list holds that are in no conversation yet.
    deliveries: Vec<Delivery>,
    /// Notes that are in a conversation and unanswered: each reaches a turn of its sub-agent
    /// that is going.
    notes: Vec<Turn>,
    /// Sub-agents a person asked for, each with the ask this server answers for it.
    starts: Vec<(Turn, Start)>,
}

/// A sub-agent its person asked for, in the conversation of an agent they keep.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Start {
    /// The agent whose conversation it was asked in: the sub-agent is started as that agent.
    by: String,
    /// The id the sub-agent gets, the same however often a server looks at the ask.
    id: String,
    name: String,
    job: String,
    /// Whether the list holds it already: only the ask is left to answer.
    held: bool,
}

/// What is known of one message in a sub-agent's conversation.
struct Seen {
    /// When it was written, in seconds.
    ts: i64,
    from_starter: bool,
}

/// What one look at a workspace's chat finds about its sub-agents.
pub(super) struct Team {
    /// The workspace's list of its agents, when it holds a sub-agent.
    roster: Option<WorkspaceAgents>,
    /// The sub-agents this server works, by the id of their own conversation.
    here: BTreeMap<String, Agent>,
    /// What the list owes a conversation, by the id its message would have, until a part
    /// holds that message.
    expected: BTreeMap<String, Delivery>,
    seen: BTreeMap<String, Seen>,
    notes: Vec<Turn>,
    /// The sub-agents asked for, by the id of the message that asks.
    starts: BTreeMap<String, Start>,
}

/// The conversation `agent` is asked in by its person: a sub-agent's own, which its item
/// names, and for an agent a person made the one that person talks to it in.
fn conversation_of(agent: &Agent) -> String {
    agent.sub.as_ref().map_or_else(
        || {
            MemberId::direct_conversation([
                MemberId::person(agent.keeper.as_str()),
                MemberId::agent(&agent.id.0),
            ])
        },
        |sub| sub.conversation.clone(),
    )
}

/// The conversation `message` is in, as Chat names it.
pub(super) fn conversation_id(message: &Message) -> &Id {
    &message.channel
}

/// The key of the cause `tail` of the sub-agent `agent` is written with: one key is one
/// message however often it is asked, so a server that restarts writes nothing twice.
fn key_of(agent: &str, tail: &str) -> String {
    let plain = format!("sub-agent:{agent}:{tail}");
    if plain.len() <= KEY_BYTES {
        return plain;
    }
    let hash = blake3::hash(plain.as_bytes()).to_hex();
    format!("sub-agent:{}", hash.get(..40).unwrap_or_default())
}

/// `text` cut to `most` bytes at a character.
fn cut(text: &str, most: usize) -> &str {
    let mut end = text.len().min(most);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default()
}

/// The turn of a lead's that `delivery` is written on, as `roster` holds the two agents now.
///
/// Only a job or a note a sub-agent gave a sub-agent of its own team has one: both are listed
/// and work for the person the message is for, and the other was started under it. The
/// lead's turns were worked in its own conversation. The list keeps who gave
/// a job or a note and not which message the lead's turn answered then, so the turn named is
/// the one the lead's own job began: every later turn of a sub-agent is work on that job.
///
/// None for everything else, which stays outside a turn: what a person's own agent gave, and
/// whatever a sub-agent would give upward, beside itself or for another person.
fn lead_turn(
    roster: &WorkspaceAgents,
    delivery: &Delivery,
) -> Option<tilo_action_policy::turn::Turn> {
    let lead = roster.get(&Id::from_raw(delivery.by.clone()))?;
    let above = lead.sub.as_ref()?;
    let under = roster.agents.iter().find(|agent| {
        agent
            .sub
            .as_ref()
            .is_some_and(|sub| sub.conversation == delivery.conversation)
    })?;
    let one_team = [lead, under]
        .iter()
        .all(|agent| agent.is_active() && agent.keeper == delivery.person)
        && roster.started_under(under, &lead.id.0);
    if !one_team {
        return None;
    }
    let job = chat::cause::message_id(
        &MemberId::person(lead.keeper.as_str()),
        &key_of(&lead.id.0, JOB),
    );
    Some(tilo_action_policy::turn::Turn::new(
        chat::summons::conversation_address(&Id::from_raw(above.conversation.clone())),
        job.to_string(),
    ))
}

/// The agent whose day `turn`'s model calls are counted on.
pub(super) fn payer(turn: &Turn) -> &Agent {
    turn.line.as_ref().map_or(&turn.agent, |line| &line.root)
}

/// The kind of work `agent`'s model is chosen by: a sub-agent's own, and for an agent a
/// person made the work an agent does for them.
pub(super) fn kind_of(agent: &Agent) -> Interaction {
    agent.sub.as_ref().map_or(Interaction::Task, |sub| {
        Interaction::from_id(&sub.kind).unwrap_or(Interaction::Subtask)
    })
}

/// What `turn`'s agent is told about sub-agents, after what every agent is told: a sub-agent
/// what it is, any other agent how it starts and steers them.
pub(super) fn told(turn: &Turn) -> String {
    // A person's own agent is told how it starts and steers a team where its turn is offered
    // one ([`super::team`]): the news it is asked with when work of a sub-agent of its ends
    // says what to do then. Its person also asks for one with `ask-agent` under the cause kind
    // `sub-agent`, which this server answers.
    let Some(sub) = &turn.agent.sub else {
        return super::team::told(turn);
    };
    let starter = turn
        .line
        .as_ref()
        .and_then(|line| line.starter.as_ref())
        .map_or(sub.started_by.as_str(), |starter| starter.name.as_str());
    let role = if sub.role.trim().is_empty() {
        String::new()
    } else {
        format!(" Your role: {}.", sub.role.trim())
    };
    format!(
        "\n\nYou are a sub-agent. {starter} started you for one job, for the person you both \
         work for.{role} Your job:\n{job}\n\nYou work in a conversation of your own. What \
         reaches you there marked (job) is that job, what is marked (note) is a note \
         {starter} gives you while you work, and anything else is your person writing to you. \
         You cannot read {starter}'s conversation or any other, and nothing Chat keeps is \
         yours to read: work from what you are told and from the workspace's other apps. When \
         the job is done, say what came of it in a few plain sentences; {starter} is told it. \
         When only your person can decide something, ask them in your answer.{team}",
        job = sub.job,
        team = super::team::told(turn),
    )
}

impl Team {
    /// What `roster` asks for the workspace's sub-agents, of which this server works those
    /// in `worked`.
    fn of(roster: Option<WorkspaceAgents>, worked: &BTreeMap<String, Agent>) -> Self {
        let mut team = Self {
            roster: None,
            here: BTreeMap::new(),
            expected: BTreeMap::new(),
            seen: BTreeMap::new(),
            notes: Vec::new(),
            starts: BTreeMap::new(),
        };
        let Some(roster) = roster else {
            return team;
        };
        for agent in roster.agents.iter().filter(|agent| agent.is_active()) {
            let Some(sub) = &agent.sub else {
                continue;
            };
            if worked.contains_key(&agent.id.0) {
                team.here.insert(sub.conversation.clone(), agent.clone());
            }
            let named = |id: &str| {
                roster
                    .get(&Id::from_raw(id.to_owned()))
                    .map_or_else(|| id.to_owned(), |by| by.name.clone())
            };
            let person = MemberId::person(agent.keeper.as_str());
            let mut owe = |by: &str, kind: &'static str, tail: &str, text: &str| {
                let key = key_of(&agent.id.0, tail);
                team.expected.insert(
                    chat::cause::message_id(&person, &key).to_string(),
                    Delivery {
                        person: agent.keeper.clone(),
                        by: by.to_owned(),
                        by_name: named(by),
                        conversation: sub.conversation.clone(),
                        kind,
                        key,
                        text: text.to_owned(),
                    },
                );
            };
            // Its job, until it stood any other way than working: then the job was told.
            if sub.stands == Stands::Working {
                owe(&sub.started_by, JOB, JOB, &sub.job);
            }
            for note in &sub.notes {
                owe(&note.by, NOTE, &format!("{NOTE}:{}", note.id), &note.text);
            }
        }
        team.roster = Some(roster);
        team
    }

    /// Take in one part of the chat's messages, the document `document`: what it holds of what
    /// the list owes, and what stands in the conversations of the sub-agents worked here.
    pub(super) fn read(&mut self, document: &str, chat: &Chat, part: &chat::parts::ChatMessages) {
        let Some(roster) = &self.roster else {
            return;
        };
        for (id, message) in part.messages.iter() {
            if let Some(start) = Self::start_of(roster, id.as_str(), message) {
                self.starts.insert(id.to_string(), start);
            }
            let delivered = self.expected.remove(id.as_str());
            let Some(agent) = self.here.get(conversation_id(message).as_str()) else {
                continue;
            };
            let me = MemberId::agent(&agent.id.0);
            // Work its line of starters gave it: its job, a note, and its person's answer to a
            // question it asked on the way.
            let from_starter = message.cause.as_ref().is_some_and(|cause| {
                let kind = cause.kind.0.as_str();
                kind == ANSWER
                    || (matches!(kind, JOB | NOTE)
                        && message
                            .author
                            .agent_id()
                            .is_some_and(|by| roster.started_under(agent, by)))
            });
            self.seen.insert(
                id.to_string(),
                Seen {
                    ts: message.ts,
                    from_starter,
                },
            );
            // A note that is in the conversation and unanswered reaches a turn that is going.
            let unanswered = chat::summons::unanswered(part.messages.get(&reply_id_at(
                id,
                &me,
                message.attempts,
            )));
            if delivered.is_some_and(|delivery| delivery.kind == NOTE) && unanswered {
                let (request, _) = chat::summons::request(message);
                let said_in = conversation_id(message);
                let told = chat::recall::told(chat, part, None, said_in, id, &me, request);
                self.notes.push(Turn {
                    agent: agent.clone(),
                    person: agent.keeper.clone(),
                    conversation: chat::summons::conversation_address(said_in),
                    cause: id.to_string(),
                    program: None,
                    request: told.request,
                    history: told.history,
                    asked: chat::summons::asked_words(message),
                    fold: told.fold,
                    typed: true,
                    line: None,
                    ask: Ask::Message {
                        part: document.to_owned(),
                        message: id.to_string(),
                        attempt: message.attempts,
                    },
                });
            }
        }
    }

    /// The sub-agent `message` asks for: a person's own ask of cause kind [`START`], in the
    /// conversation of an agent they keep. What an agent asks under that kind starts nothing:
    /// only a person asks for one this way, and the list's own rules then decide.
    fn start_of(roster: &WorkspaceAgents, id: &str, message: &Message) -> Option<Start> {
        let cause = message.cause.as_ref()?;
        if cause.kind.0 != START || message.author != cause.person {
            return None;
        }
        let person = message.author.person_id()?;
        let by = roster.agents.iter().find(|agent| {
            agent.is_active()
                && agent.keeper == person
                && conversation_of(agent) == conversation_id(message).as_str()
        })?;
        let job = message.text.trim();
        let label = cause.label.trim();
        let name = if label.is_empty() {
            job.lines().next().unwrap_or_default()
        } else {
            label
        };
        let sub_agent = format!("sub-{}", cut(id, 12));
        Some(Start {
            by: by.id.0.clone(),
            held: roster.get(&Id::from_raw(sub_agent.clone())).is_some(),
            id: sub_agent,
            name: name.chars().take(tilo_workspace_agent::MAX_NAME).collect(),
            job: job.to_owned(),
        })
    }

    /// Where `turn`, a sub-agent's, stands in its team.
    fn line_of(&self, turn: &Turn) -> Option<Line> {
        let (roster, sub) = (self.roster.as_ref()?, turn.agent.sub.as_ref()?);
        Some(Line {
            root: roster.root_of(&turn.agent).clone(),
            starter: roster.get(&Id::from_raw(sub.started_by.clone())).cloned(),
            from_starter: self
                .seen
                .get(&turn.cause)
                .is_some_and(|seen| seen.from_starter),
        })
    }

    /// Settle what the look found into `asks`: a sub-agent's turns are those in its own
    /// conversation, each knows its line; what stood before a stop on its item is stopped;
    /// and what the list still owes a conversation is owed.
    pub(super) fn settle(mut self, asks: &mut Asks) {
        if self.roster.is_none() {
            return;
        }
        let own = |turn: &Turn| {
            turn.agent.sub.as_ref().is_none_or(|sub| {
                turn.conversation
                    == chat::summons::conversation_address(&Id::from_raw(sub.conversation.clone()))
            })
        };
        let mut stopped: Vec<Turn> = std::mem::take(&mut asks.stopped)
            .into_iter()
            .filter(|turn| own(turn))
            .collect();
        let mut turns = Vec::new();
        let mut starts = Vec::new();
        for mut turn in std::mem::take(&mut asks.turns) {
            // An ask for a sub-agent is this server's to answer, not a turn of the agent's model.
            if let Some(start) = self
                .starts
                .get(&turn.cause)
                .filter(|start| start.by == turn.agent.id.0)
            {
                starts.push((turn, start.clone()));
                continue;
            }
            if !own(&turn) {
                continue;
            }
            turn.line = self.line_of(&turn);
            // A stop on its item ends what stood when it was given: what was written to it
            // since is new work.
            let before_stop = turn.agent.sub.as_ref().is_some_and(|sub| {
                sub.stands == Stands::Stopped
                    && self
                        .seen
                        .get(&turn.cause)
                        .is_none_or(|seen| seen.ts <= sub.since)
            });
            if before_stop {
                stopped.push(turn);
            } else {
                turns.push(turn);
            }
        }
        for turn in &mut stopped {
            turn.line = self.line_of(turn);
        }
        let mut notes = std::mem::take(&mut self.notes);
        for turn in &mut notes {
            turn.line = self.line_of(turn);
        }
        asks.turns = turns;
        asks.stopped = stopped;
        asks.sub = Owed {
            deliveries: self.expected.into_values().collect(),
            notes,
            starts,
        };
    }
}

impl ServerAgents {
    /// What `workspace`'s list of its agents asks for its sub-agents, of which this server
    /// works those in `worked`.
    ///
    /// # Errors
    /// The list could not be read.
    pub(super) async fn team(
        &self,
        workspace: &str,
        worked: &BTreeMap<String, Agent>,
    ) -> Result<Team, String> {
        let roster = super::roster(&self.workspaces, workspace).await?;
        Ok(Team::of(roster, worked))
    }

    /// Carry what a look found owed to the sub-agents of `workspace`: each job and note the
    /// list holds is written into its sub-agent's conversation, as the words of the agent
    /// that gave it, and a note that is there reaches the turn of its sub-agent that is going.
    ///
    /// The look read at `reading`, and a turn that began or went on since takes no note of it
    /// ([`going::Going::reach_read`]): the note may be one its agent answered before, and the
    /// turn would leave its person's newer words for it. Whether every note was still current
    /// for the turn it was handed to: where one was not, the look reads again.
    pub(super) async fn carry(&self, workspace: &str, owed: Owed, reading: going::Reading) -> bool {
        // A sub-agent its person asked for is started first: its job is owed at the next look.
        for (turn, start) in owed.starts {
            self.answer_start(workspace, &turn, &start).await;
        }
        for delivery in owed.deliveries {
            if let Err(error) = self.deliver(workspace, &delivery).await {
                tracing::warn!(%workspace, kind = delivery.kind, %error, "a sub-agent was not told what the list holds for it");
            }
        }
        let mut current = true;
        for note in owed.notes {
            // With no turn of it going the note is its next turn, as the next look gives it.
            if let going::Entered::Goes(going) = self.going.enter(workspace, &note)
                && going.reach_read(note, reading) == going::Handed::Stale
            {
                current = false;
            }
        }
        current
    }

    /// Start the sub-agent its person asked `turn`'s agent for, and answer the ask in that
    /// agent's name: no model is asked, the words of the ask are the job as they stand.
    async fn answer_start(&self, workspace: &str, turn: &Turn, start: &Start) {
        let started = if start.held {
            Ok(())
        } else {
            self.start(workspace, turn, start).await
        };
        let outcome = match started {
            Ok(()) => Outcome::Answered(format!(
                "I started the sub-agent {:?}. It works in a conversation of its own, and I tell \
                 you here when it ends.",
                start.name
            )),
            Err(why) => Outcome::Failed(why),
        };
        let answered = match self.scope(workspace, turn).await {
            Ok(acting) => acting.reply(turn).finished(&outcome).await,
            Err(error) => Err(error),
        };
        if let Err(error) = answered {
            tracing::warn!(%workspace, agent = %turn.agent.id.0, %error, "an ask for a sub-agent was not answered");
        }
    }

    /// Apply `start-sub-agent` on the list as `turn`'s agent, for the person who asked: the
    /// list's own rules decide (who keeps the agent, how deep, how many at once).
    async fn start(&self, workspace: &str, turn: &Turn, start: &Start) -> Result<(), String> {
        let document = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let (by, person) = (turn.agent.id.0.clone(), turn.person.clone());
        let sub_agent = tilo_workspace_agent::NewSubAgent {
            name: start.name.clone(),
            job: start.job.clone(),
            id: Some(start.id.clone()),
            ..tilo_workspace_agent::NewSubAgent::default()
        };
        let applied = self
            .workspaces
            .author_application(
                workspace,
                self.workspaces.governor(),
                &tilo_agent_session::AppCatalogue::native(),
                tilo_workspace_agent::OWNER,
                &document,
                |doc| {
                    let mut starting =
                        tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
                            .as_agent_for(&by, &person)
                            .in_app(tilo_workspace_agent::OWNER, "Assistant");
                    // A replica stamps what it writes with a fixed day of its own unless told the time.
                    starting.set_now_ms(tilo_clock::unix_now_ms_i64());
                    let applied = starting
                        .apply(WorkspaceAgents::START_SUB_AGENT.with(
                            tilo_workspace_agent::workspace_agents::StartSubAgent { sub_agent },
                        ))
                        .map(drop)
                        .map_err(|error| error.to_string());
                    std::mem::swap(doc, starting.doc_mut());
                    applied
                },
            )
            .await
            .map_err(|error| error.to_string());
        match applied {
            // Another look started it meanwhile: the list holds it, which is what was asked.
            Err(_) if self.holds(workspace, &start.id).await => Ok(()),
            other => other,
        }
    }

    /// Whether `workspace`'s list of its agents holds the agent `id`.
    async fn holds(&self, workspace: &str, id: &str) -> bool {
        super::roster(&self.workspaces, workspace)
            .await
            .ok()
            .flatten()
            .is_some_and(|roster| roster.get(&Id::from_raw(id.to_owned())).is_some())
    }

    /// The turn `delivery` is written on when a lead gave it to one of its own team
    /// ([`lead_turn`]); none for what any other agent gave.
    ///
    /// # Errors
    /// The list could not be read.
    async fn of_lead(
        &self,
        workspace: &str,
        delivery: &Delivery,
    ) -> Result<Option<tilo_action_policy::turn::Turn>, String> {
        let Some(roster) = super::roster(&self.workspaces, workspace).await? else {
            return Ok(None);
        };
        Ok(lead_turn(&roster, delivery))
    }

    /// Write `delivery`, a job or a note the list holds, into its sub-agent's conversation as
    /// the words of the agent that gave it.
    ///
    /// What a person's own agent gave is written in that agent's name outside a turn, where the
    /// service holds it to what the list keeps for the agent. What a sub-agent gave one of its
    /// own team is this server's to carry as well: that one message is written for the lead's
    /// turn ([`lead_turn`]), as every call of a turn this server works is, and through the
    /// service as it serves everyone, so it does what the lead's chain may.
    async fn deliver(&self, workspace: &str, delivery: &Delivery) -> Result<(), String> {
        let workspace_id = WorkspaceId::parse(workspace).ok_or("not a workspace id")?;
        let person: [u8; 32] = tilo_action_policy::PersonId::parse(&delivery.person)
            .ok()
            .and_then(|person| person.root())
            .ok_or("a sub-agent works for a person")?;
        let agents = self.agents(workspace).await?;
        let by = agents
            .get(&delivery.by)
            .ok_or("the agent that gave it no longer works here")?;
        let caller = self.agent_caller(workspace_id, by, person).await?;
        let caller = match self.of_lead(workspace, delivery).await? {
            Some(turn) => caller.on_roster_turn(turn),
            None => caller,
        };
        let service = WorkspaceActionProvider::new(self.actions.clone(), workspace_id);
        let label = cut(delivery.by_name.trim(), LABEL_BYTES);
        ask_agent(
            &service,
            &caller,
            workspace,
            (&delivery.conversation, &delivery.text),
            (
                delivery.kind,
                if label.is_empty() {
                    delivery.by.as_str()
                } else {
                    label
                },
                &delivery.key,
            ),
        )
        .await
    }

    /// Say on its item how the sub-agent `agent` stands, as the sub-agent itself. The server
    /// that works it writes the item, as it writes what an agent spent: the list lets only a
    /// sub-agent's own runner say it, and no tool of a turn is asked for it.
    async fn stands(&self, workspace: &str, agent: &Agent, stands: Stands, line: &str) {
        let document = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let (id, keeper) = (agent.id.0.clone(), agent.keeper.clone());
        let item = tilo_resource::Id::from_raw(id.clone());
        let line = cut(line.trim(), sub::MAX_LINE.saturating_mul(2)).to_owned();
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
                            .as_agent_for(&id, &keeper)
                            .in_app(tilo_workspace_agent::OWNER, "Assistant");
                    // A replica stamps what it writes with a fixed day of its own unless told the time.
                    saying.set_now_ms(tilo_clock::unix_now_ms_i64());
                    let applied = saying
                        .apply(WorkspaceAgents::SUB_AGENT_STANDS.with(
                            tilo_workspace_agent::workspace_agents::SubAgentStands {
                                agent: item,
                                stands,
                                line: (!line.is_empty()).then_some(line),
                            },
                        ))
                        .map(drop)
                        .map_err(|error| error.to_string());
                    std::mem::swap(doc, saying.doc_mut());
                    applied
                },
            )
            .await;
        if let Err(error) = written {
            tracing::warn!(%workspace, agent = %agent.id.0, %error, "how a sub-agent stands was not written");
        }
    }

    /// A turn of a sub-agent begins: its item says it works.
    pub(super) async fn sub_began(&self, workspace: &str, turn: &Turn) {
        let Some(sub) = &turn.agent.sub else {
            return;
        };
        if sub.stands != Stands::Working || !sub.line.is_empty() {
            self.stands(workspace, &turn.agent, Stands::Working, "")
                .await;
        }
    }

    /// Whether a question of the sub-agent's own waits for its person in its conversation, in
    /// the part `turn` was asked in or one begun since.
    async fn asks_its_person(&self, workspace: &str, turn: &Turn) -> bool {
        let (Some(sub), Ask::Message { part, .. }) = (&turn.agent.sub, &turn.ask) else {
            return false;
        };
        let Some(asked_in) = collab_types::document::DocumentId::parse(part)
            .and_then(|part| chat::parts::part_index(&part))
        else {
            return false;
        };
        let me = MemberId::agent(&turn.agent.id.0);
        for index in asked_in..asked_in.saturating_add(4) {
            let Ok(Some(held)) = self
                .read::<chat::parts::ChatMessages>(
                    workspace,
                    CHAT,
                    chat::parts::part(index).as_str(),
                )
                .await
            else {
                break;
            };
            let waits = held.messages.iter().any(|(_, message)| {
                message.author == me
                    && conversation_id(message).as_str() == sub.conversation
                    && message
                        .question
                        .as_ref()
                        .is_some_and(chat::question::Question::waiting)
            });
            if waits {
                return true;
            }
        }
        false
    }

    /// A turn of a sub-agent ended as `outcome`: its item says how it stands now, and when
    /// the work came from its line of starters (`from_starter`), its starter is told.
    pub(super) async fn sub_ended(
        &self,
        acting: &super::TurnScope,
        turn: &Turn,
        outcome: &Outcome,
        from_starter: bool,
    ) {
        if turn.agent.sub.is_none() {
            return;
        }
        let (stands, said) = match outcome {
            Outcome::Answered(words) => {
                let stands = if self.asks_its_person(&acting.workspace, turn).await {
                    Stands::NeedsPerson
                } else {
                    Stands::Done
                };
                (stands, words.as_str())
            }
            // It stopped at a limit only its person can raise, or needs to be told how to go on.
            Outcome::Stopped(words) => (Stands::NeedsPerson, words.as_str()),
            Outcome::Failed(words) => (Stands::Failed, words.as_str()),
            Outcome::Cancelled => (Stands::Stopped, ""),
        };
        self.stands(&acting.workspace, &turn.agent, stands, said)
            .await;
        let from_starter = from_starter || turn.line.as_ref().is_some_and(|line| line.from_starter);
        if !from_starter || stands == Stands::Stopped {
            return;
        }
        // The server's own write, so through the service as it serves everyone: the one kept to
        // the sub-agent's conversation takes of Chat only the three writes of its turn.
        if let Err(error) = self
            .tell_starter(
                &acting.workspace,
                (
                    &acting.agent,
                    &WorkspaceActionProvider::new(self.actions.clone(), acting.workspace_id),
                ),
                turn,
                (stands, said),
            )
            .await
        {
            tracing::warn!(workspace = %acting.workspace, agent = %turn.agent.id.0, %error, "a sub-agent's starter was not told how its work ended");
        }
    }

    /// Tell the agent that started `turn`'s sub-agent how its work ended, in the starter's own
    /// conversation, as the sub-agent's news: the starter answers it there like any ask.
    ///
    /// The server writes it for the turn that ended, so the caller carries that turn, as every
    /// call of a turn this server works does.
    /// Outside a turn nothing is taken in a sub-agent's name but what its own rules let it, and
    /// asking an agent is none of that: this one message upward is the server's, never a call
    /// of the sub-agent's model, whose turn is served kept to its own conversation.
    async fn tell_starter(
        &self,
        workspace: &str,
        (agent, service): (&Caller, &WorkspaceActionProvider),
        turn: &Turn,
        (stands, said): (Stands, &str),
    ) -> Result<(), String> {
        let (Some(sub), Some(line)) = (&turn.agent.sub, &turn.line) else {
            return Ok(());
        };
        let (
            Some(starter),
            Ask::Message {
                message, attempt, ..
            },
        ) = (&line.starter, &turn.ask)
        else {
            return Ok(());
        };
        let how = match stands {
            Stands::Done => "done",
            Stands::Failed => "failed",
            Stands::NeedsPerson => "needs its person",
            Stands::Working | Stands::Stopped => return Ok(()),
        };
        // The starter's own conversation: a sub-agent's is on its item, and a person's own
        // agent is asked where that person talks to it.
        let theirs = conversation_of(starter);
        let said: String = said.trim().chars().take(TOLD_CHARS).collect();
        let said = if said.is_empty() {
            "(it said nothing)".to_owned()
        } else {
            said
        };
        let then = if starter.is_sub() {
            "Take it into your own job."
        } else {
            "Tell your person in one line what came of it."
        };
        let text = format!(
            "Your sub-agent {name:?} (`{id}`) ended the work you gave it and now stands: {how}.\n\
             Its job: {job}\nWhat it said:\n{said}\n\n{then} The whole of it is in its \
             conversation, `{conversation}`.",
            name = turn.agent.name,
            id = turn.agent.id.0,
            job = sub.job.lines().next().unwrap_or_default(),
            conversation = turn.conversation,
        );
        let caller = agent
            .clone()
            .on_roster_turn(tilo_action_policy::turn::Turn::new(
                turn.conversation.clone(),
                turn.cause.clone(),
            ));
        let label = format!(
            "{}: {how}",
            cut(turn.agent.name.trim(), LABEL_BYTES.saturating_sub(24))
        );
        ask_agent(
            service,
            &caller,
            workspace,
            (&theirs, &text),
            (
                chat::cause::NEWS,
                &label,
                &key_of(&turn.agent.id.0, &format!("{message}:{attempt}")),
            ),
        )
        .await
    }
}

/// Ask the agents of the conversation `conversation` with `text` through Chat's `ask-agent`, as
/// `caller`, under the cause (`kind`, `label`, `key`).
async fn ask_agent(
    service: &WorkspaceActionProvider,
    caller: &Caller,
    workspace: &str,
    (conversation, text): (&str, &str),
    (kind, label, key): (&str, &str, &str),
) -> Result<(), String> {
    let route = DocumentRoute {
        app_id: CHAT.to_owned(),
        cluster_id: workspace.to_owned(),
        document: String::new(),
    };
    let mut arguments = Map::new();
    arguments.insert("text".into(), json!(text));
    arguments.insert(TO.into(), json!(conversation));
    arguments.insert(
        "cause".into(),
        json!({"kind": kind, "label": label, "key": key}),
    );
    invoke_named(
        service,
        caller,
        &route,
        &collab_types::document::chain_resource_id(&chat::parts::chain()),
        "ask-agent",
        arguments,
    )
    .await
    .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cause_key_names_its_sub_agent_and_stays_within_what_a_cause_carries() {
        assert_eq!(key_of("researcher", JOB), "sub-agent:researcher:job");
        assert_eq!(
            key_of("researcher", "note:n1"),
            "sub-agent:researcher:note:n1"
        );
        let long = "a".repeat(400);
        let key = key_of(&long, JOB);
        assert!(key.len() <= KEY_BYTES, "{key}");
        assert_eq!(key, key_of(&long, JOB), "the same every time it is made");
        assert_ne!(key, key_of(&long, "note:n1"));
        assert_eq!(cut("äöü", 3), "ä", "cut at a character");
    }

    /// A person's own agent, a lead it started, the lead's worker and a sub-agent beside the
    /// lead, each made by the list's own actions.
    fn team() -> WorkspaceAgents {
        use tilo_workspace_agent::workspace_agents::{
            CreateAgent, SetSubAgentLimits, StartSubAgent,
        };
        let start = |name: &str, job: &str| {
            WorkspaceAgents::START_SUB_AGENT.with(StartSubAgent {
                sub_agent: tilo_workspace_agent::NewSubAgent {
                    name: name.to_owned(),
                    job: job.to_owned(),
                    ..tilo_workspace_agent::NewSubAgent::default()
                },
            })
        };
        let mut list = tilo_resource::Replica::<WorkspaceAgents>::new().as_author("ada");
        list.set_now_ms(1_700_000_000_000);
        list.apply(WorkspaceAgents::CREATE_AGENT.with(CreateAgent {
            name: tilo_resource::NonBlank::parse("Assistant").expect("a name"),
            id: Some("main".to_owned()),
            model: None,
            mcp_client: None,
        }))
        .expect("a person makes their own agent");
        list.apply(
            WorkspaceAgents::SET_SUB_AGENT_LIMITS.with(SetSubAgentLimits {
                agent: Id::from_raw("main"),
                depth: Some(2),
                at_once: None,
            }),
        )
        .expect("its keeper lets sub-agents go two deep");
        let mut list = list.as_agent_for("main", "ada");
        list.apply(start("Lead", "Build the feature"))
            .expect("the person's own agent starts a lead");
        list.apply(start("Other", "Find the deck"))
            .expect("and one beside it");
        let mut list = list.as_agent_for("lead", "ada");
        list.apply(start("Worker", "Write the tests"))
            .expect("the lead starts its worker");
        list.state().expect("the list reads")
    }

    /// `list` with its lead changed by `change`.
    fn with_lead(list: &WorkspaceAgents, change: impl Fn(&mut Agent)) -> WorkspaceAgents {
        let mut agents: Vec<Agent> = list.agents.iter().cloned().collect();
        for agent in &mut agents {
            if agent.id.0 == "lead" {
                change(agent);
            }
        }
        WorkspaceAgents {
            agents: agents.into(),
        }
    }

    #[test]
    fn a_job_is_written_on_its_leads_turn_only_for_the_leads_own_team() {
        let list = team();
        let own = |agent: &str| {
            list.get(&Id::from_raw(agent))
                .and_then(|agent| agent.sub.as_ref())
                .map(|sub| sub.conversation.clone())
                .expect("a sub-agent's own conversation")
        };
        let given = |by: &str, to: &str, person: &str| Delivery {
            person: person.to_owned(),
            by: by.to_owned(),
            by_name: by.to_owned(),
            conversation: to.to_owned(),
            kind: JOB,
            key: key_of("worker", JOB),
            text: "Write the tests".to_owned(),
        };
        let to_worker = given("lead", &own("worker"), "ada");

        // A lead's job to its worker: the lead's turn in the lead's own conversation, the one its
        // own job began.
        let turn = lead_turn(&list, &to_worker)
            .expect("a lead's job to its worker is written on the lead's turn");
        assert_eq!(
            turn.conversation,
            chat::summons::conversation_address(&Id::from_raw(own("lead")))
        );
        assert_eq!(
            turn.message,
            chat::cause::message_id(&MemberId::person("ada"), &key_of("lead", JOB)).to_string()
        );
        // Everything else stays outside a turn, where the list's own rules for the agent decide.
        for (what, by, to, person) in [
            (
                "a person's own agent gives a job outside a turn",
                "main",
                own("lead"),
                "ada",
            ),
            (
                "a worker gives nothing upward",
                "worker",
                own("lead"),
                "ada",
            ),
            (
                "a lead gives nothing to one beside it",
                "lead",
                own("other"),
                "ada",
            ),
            ("nor to itself", "lead", own("lead"), "ada"),
            (
                "nor into a conversation that is no sub-agent's",
                "lead",
                "direct:main".to_owned(),
                "ada",
            ),
            ("nor for another person", "lead", own("worker"), "grace"),
        ] {
            assert!(
                lead_turn(&list, &given(by, &to, person)).is_none(),
                "{what}"
            );
        }
        let retired = with_lead(&list, |lead| {
            lead.state = tilo_workspace_agent::AgentState::Retired;
        });
        assert!(
            lead_turn(&retired, &to_worker).is_none(),
            "a lead that was retired gives nothing on a turn"
        );
    }
}
