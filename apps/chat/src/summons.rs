//! Who is asked what in a workspace's conversations: the person's messages an agent has to answer.
//!
//! An agent works inside a turn: a person's message that asks it ([`tilo_action_policy::turn`]).
//! A message asks the agents that take part in its conversation, and the agents it `@`mentions
//! wherever it is written, so naming an agent in a channel brings it in for that message. The
//! agent's answer is the reply its id and the message's derive ([`crate::record::reply_id_at`]):
//! a turn goes on until that reply exists and its work is finished, and a person's Retry asks the
//! same message again in a later attempt. Nothing here keeps state of its own: Chat's record says
//! what is asked and what was answered, so every reader — a person's computer running the agents
//! they keep, the workspace's host running the agents it runs — finds the same turns.

use ui::permissions::MemberId;

use crate::parts::ChatPart;
use crate::record::{Chat, Message, reply_id_at};
use ui::prelude::Id;
use ui::tilo_resource::Resource as _;

/// A person's message an agent has yet to answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summons {
    /// The conversation it is in.
    pub conversation: Id,
    /// The message.
    pub message: Id,
    /// The agent asked, by its bare id.
    pub agent: String,
    /// The person who asked, by their person id: the agent acts for them.
    pub person: String,
    /// Which time the person asked it: a Retry asks the same message again.
    pub attempt: u32,
    /// Whether the agent was named in the message rather than taking part in the conversation.
    pub mentioned: bool,
}

impl Summons {
    /// The conversation's address in the workspace, as the shell and the apps name it.
    #[must_use]
    pub fn address(&self) -> String {
        conversation_address(&self.conversation)
    }

    /// The reply that answers it: the one its id and the agent's derive.
    #[must_use]
    pub fn reply(&self) -> Id {
        reply_id_at(&self.message, &MemberId::agent(&self.agent), self.attempt)
    }
}

/// The address of conversation `id` in the workspace.
#[must_use]
pub fn conversation_address(id: &Id) -> String {
    format!("{}/channels/{id}", Chat::KIND)
}

/// The person a message's author names, when it names one: a message a person posted asks, and
/// the run answers for that person; software names nobody.
#[must_use]
pub fn person_of(author: &MemberId) -> Option<String> {
    author
        .person_id()
        .filter(|person| !person.is_empty())
        .map(str::to_owned)
}

/// Whether `reply` leaves its turn going: there is none yet, or its work has not finished.
#[must_use]
pub fn unanswered(reply: Option<&Message>) -> bool {
    reply.is_none_or(|reply| {
        reply.work.as_ref().is_some_and(|work| {
            let streamed = work
                .stream
                .as_ref()
                .is_some_and(|stream| stream.finish.is_some());
            !streamed
                && work
                    .state
                    .as_ref()
                    .is_none_or(|transition| !transition.state.finished())
        })
    })
}

/// The agents `message` asks by naming them: each mention of an agent, once.
fn mentioned_agents(message: &Message) -> Vec<String> {
    let mut agents = Vec::new();
    for mention in &message.mentions {
        let Some(agent) = mention.user.agent_id().map(str::to_owned) else {
            continue;
        };
        if !agents.contains(&agent) {
            agents.push(agent);
        }
    }
    agents
}

/// Every turn `chat`'s conversations give, read from `part`'s messages: for each conversation and
/// agent it asks, the person's newest message the agent has not answered. An archived
/// conversation asks nothing.
#[must_use]
pub fn summonses(chat: &Chat, part: &ChatPart) -> Vec<Summons> {
    let mut conversations: Vec<Id> = Vec::new();
    for (_, message) in part.messages.iter() {
        if !conversations.contains(&message.channel) {
            conversations.push(message.channel.clone());
        }
    }
    let mut out = Vec::new();
    for id in &conversations {
        if chat
            .channels
            .get(id)
            .is_some_and(|conversation| conversation.archived)
        {
            continue;
        }
        let messages: Vec<(&Id, &Message)> = part
            .messages
            .iter()
            .filter(|(_, message)| message.channel == *id)
            .collect();
        // The agents asked here: those taking part, then those a message names.
        let mut asked: Vec<(String, bool)> = chat
            .conversation_members(id)
            .into_iter()
            .filter(|participant| !participant.is_person())
            .filter_map(|participant| {
                participant
                    .agent_id()
                    .map(|agent| (agent.to_owned(), false))
            })
            .collect();
        for (_, message) in &messages {
            for agent in mentioned_agents(message) {
                if !asked.iter().any(|(held, _)| *held == agent) {
                    asked.push((agent, true));
                }
            }
        }
        for (agent, by_mention) in asked {
            let key = MemberId::agent(&agent);
            let newest = messages
                .iter()
                .copied()
                .filter(|(message_id, message)| {
                    (!message.text.trim().is_empty() || !message.attachments.is_empty())
                        && person_of(&message.author).is_some()
                        && (!by_mention || mentioned_agents(message).contains(&agent))
                        && unanswered(part.messages.get(&reply_id_at(
                            message_id,
                            &key,
                            message.attempts,
                        )))
                })
                .max_by_key(|(message_id, message)| (message.ts, message_id.as_str()));
            let Some((message_id, message)) = newest else {
                continue;
            };
            let Some(person) = person_of(&message.author) else {
                continue;
            };
            out.push(Summons {
                conversation: id.clone(),
                message: message_id.clone(),
                agent,
                person,
                attempt: message.attempts,
                mentioned: by_mention,
            });
        }
    }
    out
}

/// What `message` asks of the agents it summons, in words: its own, under the words it quotes,
/// with what it carries said beside them, and the first record it carries as the run's subject.
#[must_use]
pub fn request(message: &Message) -> (String, Option<tilo_actions::SelectionSubject>) {
    let mut subject = None;
    // A reply that answers a run of another message is asked with the words it quotes above its
    // own: the quote is the conversation's record of what "that one" is.
    let mut instruction = message
        .quoted
        .as_ref()
        .filter(|quoted| !quoted.text.trim().is_empty())
        .map_or_else(
            || message.text.trim().to_owned(),
            |quoted| format!("> {}\n\n{}", quoted.text.trim(), message.text.trim()),
        );
    for attached in &message.attachments {
        let named = attached.subject();
        if subject.is_none() && named.is_some() {
            subject = named;
            continue;
        }
        let line = match (named, attached) {
            (Some(named), _) => format!(
                "It also carries {:?} ({} `{}`, in `{}`).",
                attached.label(),
                named.resource.kind,
                named.resource.id,
                named.app_id
            ),
            (None, tilo_reference::Attached::Text { text, .. }) => {
                format!("It quotes: {text:?}.")
            }
            (None, _) => format!("It carries the file {:?}.", attached.label()),
        };
        if !instruction.is_empty() {
            instruction.push('\n');
        }
        instruction.push_str(&line);
    }
    (instruction, subject)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, reason = "in a test this is the assertion")]

    use super::*;
    use crate::model::AgentWorkState;

    const ADA: &str = "ada";

    fn said(channel: &Id, author: MemberId, text: &str, ts: i64) -> Message {
        Message::new(channel.clone(), author, ts, text.to_owned())
    }

    /// A message asks the agents of its conversation and the agents it names; the agent's
    /// finished reply ends the ask.
    #[test]
    fn a_message_asks_the_agents_in_its_conversation_and_those_it_names() {
        let chat = Chat::default();
        let mut part = ChatPart::default();
        let direct = Chat::direct_id([MemberId::person(ADA), MemberId::agent("scout")]);
        part.messages.push(
            Id::from_raw("m1"),
            said(
                &direct,
                MemberId::person(ADA),
                "Summarise the launch notes",
                1,
            ),
        );
        let general = Id::from_raw("c_general");
        let mut naming = said(
            &general,
            MemberId::person(ADA),
            "@Planner draft the agenda",
            2,
        );
        naming.mentions = vec![crate::mention::Mention {
            user: MemberId::agent("planner"),
            start: 0,
            len: 8,
            is_agent: true,
        }];
        part.messages.push(Id::from_raw("m2"), naming);
        part.messages.push(
            Id::from_raw("m3"),
            said(&general, MemberId::person(ADA), "Lunch at noon", 3),
        );

        let mut asked = summonses(&chat, &part);
        asked.sort_by(|one, other| one.agent.cmp(&other.agent));
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert_eq!(
            (
                asked[0].agent.as_str(),
                asked[0].message.as_str(),
                asked[0].mentioned
            ),
            ("planner", "m2", true),
            "naming an agent in a channel asks it that message, not the others"
        );
        assert_eq!(
            (
                asked[1].agent.as_str(),
                asked[1].message.as_str(),
                asked[1].mentioned
            ),
            ("scout", "m1", false)
        );
        assert_eq!(asked[1].person, ADA);

        // The agent's reply, finished, answers it.
        let mut reply = said(&direct, MemberId::agent("scout"), "Here is the summary", 4);
        reply.work = Some(crate::record::Work {
            state: Some(crate::record::Transition {
                state: AgentWorkState::Done { did: String::new() },
                since: 4,
            }),
            ..crate::record::Work::default()
        });
        part.messages.push(asked[1].reply(), reply);
        let left = summonses(&chat, &part);
        assert_eq!(left.len(), 1, "{left:?}");
        assert_eq!(left[0].agent, "planner");
    }
}
