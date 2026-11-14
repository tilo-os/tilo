//! The chat record as the window and the agents read it: the workspace's
//! channels and messages in the shapes [`crate::model`] draws, and the
//! composers' drafts, which are rich text in the document.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use tilo_actions::policy::MemberId;
use ui::tilo_resource::{Id, Identified, Resource};

use crate::model::{AgentStep, AgentWork, Category, Channel, EditRecord, Message, User};
use crate::record::{Chat, Conversation};

/// What the workspace calls each of its agents, by the bare id its actor carries
/// (`agent:<id>`): the answer of the workspace's own record of its agents.
pub type AgentNames = HashMap<String, String>;

/// The published id of the item `id` of the record's `field`
/// (`channels`, `messages`, …): what a caller addresses its actions to.
#[must_use]
pub fn item_id(field: &str, id: &str) -> String {
    ui::tilo_resource::items::item_resource_id::<Chat>(field, &Id::from_raw(id))
}

/// The chat record in `doc`; an empty one when `doc` holds none.
#[must_use]
pub fn read(doc: &AutoCommit) -> Chat {
    Chat::read(doc).unwrap_or_default()
}

impl Chat {
    /// The categories, in sidebar order.
    #[must_use]
    pub fn category_rows(&self) -> Vec<Category> {
        self.categories
            .iter()
            .enumerate()
            .map(|(order, (id, category))| Category {
                id: id.to_string(),
                name: category.name.clone(),
                order: u32::try_from(order).unwrap_or(u32::MAX),
                collapsed: false,
            })
            .collect()
    }

    /// The workspace's channels, in sidebar order; unread counts are the
    /// reader's own.
    #[must_use]
    pub fn channel_rows(&self) -> Vec<Channel> {
        let rows: Vec<Channel> = self
            .channels
            .iter()
            .filter(|(_, channel)| channel.is_channel())
            .enumerate()
            .map(|(order, (id, channel))| Channel {
                id: id.to_string(),
                category: channel
                    .category
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                name: channel.name.clone(),
                topic: channel.topic.clone(),
                kind: channel.kind,
                order: u32::try_from(order).unwrap_or(u32::MAX),
                unread: 0,
                mentions: 0,
                archived: channel.archived,
                direct: false,
                participants: Vec::new(),
            })
            .collect();
        rows
    }

    /// Every conversation `me` has, in sidebar order: the workspace's
    /// channels, and one direct conversation with each of the workspace's
    /// other people — the same one on every device, because its id names
    /// the participants ([`Chat::direct_id`]) — with the conversations the
    /// record keeps for an agent among them.
    ///
    /// A conversation with an agent is named by what the workspace calls its agents (`agents`),
    /// whether or not the agent is working here this moment.
    #[must_use]
    pub fn conversation_rows(
        &self,
        me: &str,
        messages: &crate::messages::Messages,
        agents: &AgentNames,
    ) -> Vec<Channel> {
        let mut rows = self.channel_rows();
        let mut direct: Vec<(String, Channel)> = Vec::new();
        let mut push = |id: &Id, participants: Vec<String>, recorded: Option<&Conversation>| {
            let name = self.display_names(id, Some(me), agents);
            direct.push((
                name.clone(),
                Channel {
                    id: id.to_string(),
                    category: String::new(),
                    name,
                    topic: recorded.map(|row| row.topic.clone()).unwrap_or_default(),
                    kind: recorded.map_or(crate::model::ChannelKind::Text, |row| row.kind),
                    order: 0,
                    unread: 0,
                    mentions: 0,
                    archived: recorded.is_some_and(|row| row.archived),
                    direct: true,
                    participants,
                },
            ));
        };
        for (id, channel) in self.channels.iter() {
            if !channel.is_channel() {
                let keys = channel.participants.iter().map(|key| key.0.clone());
                push(id, keys.collect(), Some(channel));
            }
        }
        let mut listed: std::collections::HashSet<Id> = std::collections::HashSet::new();
        for person in &self.people {
            if person.is_agent() || person.id.0 == me {
                continue;
            }
            let pair = [
                MemberId::person(me.to_owned()),
                MemberId::person(person.id.0.clone()),
            ];
            let id = Chat::direct_id(pair.clone());
            if !self.channels.contains(&id) && listed.insert(id.clone()) {
                push(&id, pair.iter().map(MemberId::key).collect(), None);
            }
        }
        // A conversation written to by its id alone (one with an agent that a send opened) is
        // listed once it holds a message, though the record keeps no row for it.
        for (_, message) in messages.iter().chain(messages.sending()) {
            let id = &message.channel;
            if self.channels.contains(id) || listed.contains(id) {
                continue;
            }
            let Some(participants) = Chat::direct_participants(id) else {
                continue;
            };
            if participants
                .iter()
                .any(|participant| participant.is_person() && participant.id() == me)
            {
                listed.insert(id.clone());
                push(id, participants.iter().map(MemberId::key).collect(), None);
            }
        }
        direct.sort_by(|left, right| {
            left.0
                .to_lowercase()
                .cmp(&right.0.to_lowercase())
                .then_with(|| left.1.id.cmp(&right.1.id))
        });
        let first = rows.len();
        for (offset, (_, mut row)) in direct.into_iter().enumerate() {
            row.order = u32::try_from(first.saturating_add(offset)).unwrap_or(u32::MAX);
            rows.push(row);
        }
        rows
    }

    /// What the conversation is called: its own name when it has one, and
    /// else the names of the people it is with, `me`'s left out when they
    /// are one of them and someone else is. A person the workspace has no
    /// name for goes by their tag, never blank.
    ///
    /// The one place a conversation is named, for the window and for whatever
    /// else names it (an agent's run on a turn in it).
    ///
    /// An agent is named by what the workspace's own record calls it (`agents`), the identity
    /// record every app of the workspace reads, not by whether its device happens to be here.
    #[must_use]
    pub fn display_names(&self, id: &Id, me: Option<&str>, agents: &AgentNames) -> String {
        if let Some(conversation) = self.channels.get(id)
            && !conversation.participants.is_empty()
            && !conversation.name.trim().is_empty()
        {
            return conversation.name.trim().to_owned();
        }
        let participants = self.conversation_participants(id);
        let named: Vec<String> = participants
            .iter()
            .map(|participant| {
                let agent_name = if participant.is_person() {
                    None
                } else {
                    agents
                        .get(participant.id())
                        .map(|name| name.trim().to_owned())
                        .filter(|name| !name.is_empty())
                };
                agent_name
                    .or_else(|| {
                        self.people
                            .iter()
                            .find(|person| person.id.0 == participant.key())
                            .map(|person| person.name.trim().to_owned())
                            .filter(|name| !name.is_empty())
                    })
                    .unwrap_or_else(|| crate::identity::unnamed_label(participant.id()))
            })
            .collect();
        let others: Vec<String> = participants
            .iter()
            .zip(named.iter())
            .filter(|(participant, _)| Some(participant.id()) != me)
            .map(|(_, name)| name.clone())
            .collect();
        let shown = if others.is_empty() { named } else { others };
        shown.join(", ")
    }

    /// The people and agents in the workspace.
    #[must_use]
    pub fn user_rows(&self) -> Vec<User> {
        self.people
            .iter()
            .map(|person| User {
                id: person.id.0.clone(),
                name: person.name.clone(),
                initials: person.initials.clone(),
                color_idx: usize::try_from(person.color).unwrap_or_default(),
                status: person.status,
                is_agent: person.is_agent(),
            })
            .collect()
    }

    /// The names hosts gave their agent tasks.
    #[must_use]
    pub fn agent_authors(&self) -> HashMap<String, String> {
        self.agent_names
            .iter()
            .filter(|(participant, name)| {
                participant.starts_with(tilo_actions::AGENT_TASK_PARTICIPANT_PREFIX)
                    && !name.trim().is_empty()
            })
            .map(|(participant, name)| (participant.clone(), name.clone()))
            .collect()
    }

    /// The rows of `channel`, read from the parts' rows already read (`parts`,
    /// oldest first) instead of from the chain: the open channel's messages,
    /// with the feedback filed in it where it was filed and what is being sent
    /// after them. A reload reads the parts that changed and no others, and
    /// the rows of those stand here as they were read.
    #[must_use]
    pub fn rows_from_parts(
        &self,
        parts: &[(u64, std::rc::Rc<Vec<Message>>)],
        messages: &crate::messages::Messages,
        channel: &str,
    ) -> Vec<Message> {
        rows_from_parts(&self.feedback_rows(channel), parts, messages, channel)
    }

    /// The messages of `channel` held in `messages`, oldest first, with the
    /// feedback filed in it among them where it was filed, and the messages
    /// being sent to it after them.
    #[must_use]
    pub fn message_rows(
        &self,
        messages: &crate::messages::Messages,
        channel: &str,
    ) -> Vec<Message> {
        let rows = messages
            .iter()
            .filter(|(_, message)| message.channel.as_str() == channel)
            .map(|(id, message)| message_row(id, message))
            .collect();
        with_filed_and_sending(rows, &self.feedback_rows(channel), messages, channel)
    }

    /// The feedback filed in `channel`, as the timeline shows it: a message
    /// by whoever filed it, saying what they reported
    /// ([`crate::feedback::message`]).
    fn feedback_rows(&self, channel: &str) -> Vec<Message> {
        self.feedback_messages()
            .filter(|(_, message)| message.channel.as_str() == channel)
            .map(|(id, message)| message_row(&id, &message))
            .collect()
    }

    /// Every filed report as the message its channel shows, under its
    /// message id ([`crate::feedback::message_id`]).
    pub fn feedback_messages(&self) -> impl Iterator<Item = (Id, crate::record::Message)> + '_ {
        self.feedback.iter().filter_map(|(id, report)| {
            Some((
                Id::from_raw(crate::feedback::message_id(id.as_str())),
                crate::feedback::message::message(report)?,
            ))
        })
    }

    /// The message `id` held in `messages`, in whichever channel it is, or
    /// the feedback report it shows.
    #[must_use]
    pub fn message_row(&self, messages: &crate::messages::Messages, id: &str) -> Option<Message> {
        let key = Id::from_raw(id);
        messages
            .get(&key)
            .map(|message| message_row(&key, message))
            .or_else(|| {
                self.feedback.iter().find_map(|(report, filed)| {
                    let submission = filed.submission.as_ref()?;
                    (crate::feedback::message_id(report.as_str()) == id)
                        .then(|| self.feedback_rows(&submission.destination.channel))?
                        .into_iter()
                        .find(|row| row.id == id)
                })
            })
    }

    /// How far `user` read `channel`.
    #[must_use]
    pub fn read_mark(&self, channel: &str, user: &str) -> crate::parts::ReadMark {
        self.read
            .get(user)
            .and_then(|marks| marks.get(channel))
            .copied()
            .unwrap_or_default()
    }

    /// The top-level messages of `channel` held in `messages` that `user`
    /// has not seen. In a direct conversation, what they wrote themselves is
    /// never unread for them.
    #[must_use]
    pub fn unread(&self, messages: &crate::messages::Messages, channel: &str, user: &str) -> u32 {
        let mark = self.read_mark(channel, user);
        let unread = if self.is_direct_conversation(channel) {
            messages.unread_by_others(&Id::from_raw(channel), mark, user)
        } else {
            messages.unread(&Id::from_raw(channel), mark)
        };
        u32::try_from(unread).unwrap_or(u32::MAX)
    }

    /// Whether `channel` is a conversation some people have rather than one
    /// of the workspace's channels.
    #[must_use]
    pub fn is_direct_conversation(&self, channel: &str) -> bool {
        let id = Id::from_raw(channel);
        Chat::is_direct(&id)
            || self
                .channels
                .get(&id)
                .is_some_and(|conversation| !conversation.is_channel())
    }
}

/// One message as the timeline's row for it.
#[must_use]
pub fn row_of(id: &Id, message: &crate::record::Message) -> Message {
    message_row(id, message)
}

/// [`Chat::rows_from_parts`] over rows already read, with the `filed` feedback
/// reports put where they were filed and what is being sent after them.
fn rows_from_parts(
    filed: &[Message],
    parts: &[(u64, std::rc::Rc<Vec<Message>>)],
    messages: &crate::messages::Messages,
    channel: &str,
) -> Vec<Message> {
    let mut rows: Vec<Message> = Vec::new();
    // What the record kept in itself comes before any part: the oldest of the
    // timeline, and what a device that holds no part of it still shows.
    for (id, message) in messages.kept() {
        if message.channel.as_str() == channel && messages.part_of(id).is_none() {
            rows.push(message_row(id, message));
        }
    }
    for (_, part) in parts {
        rows.extend(
            part.iter()
                .filter(|message| message.channel.as_str() == channel)
                .cloned(),
        );
    }
    with_filed_and_sending(rows, filed, messages, channel)
}

/// `rows` with the `filed` feedback reports put where they were filed and what is being sent to
/// `channel` after them.
fn with_filed_and_sending(
    mut rows: Vec<Message>,
    filed: &[Message],
    messages: &crate::messages::Messages,
    channel: &str,
) -> Vec<Message> {
    for report in filed {
        // Where it was filed: before the first message written after it, in
        // the timeline's own order.
        let at = rows
            .iter()
            .position(|row| row.ts > report.ts)
            .unwrap_or(rows.len());
        rows.insert(at, report.clone());
    }
    rows.extend(
        messages
            .sending()
            .filter(|(_, message)| message.channel.as_str() == channel)
            .map(|(id, message)| Message {
                sending: true,
                ..message_row(id, message)
            }),
    );
    rows
}

pub(crate) fn message_row(id: &Id, message: &crate::record::Message) -> Message {
    let reactions = message.reactions.groups();
    Message {
        id: id.to_string(),
        channel: message.channel.to_string(),
        author: message.author.0.clone(),
        ts: message.ts,
        text: message.text.clone(),
        body: message
            .body
            .iter()
            .map(tilo_rich_text::content_of_clip)
            .collect(),
        edited: message.edited,
        history: message
            .history
            .iter()
            .map(|revision| EditRecord {
                ts: revision.ts,
                text: revision.text.clone(),
            })
            .collect(),
        parent: message.parent.as_ref().map(ToString::to_string),
        quoted: message.quoted.clone(),
        reactions,
        mentions: message.mentions.clone(),
        attachments: message.attachments.clone(),
        pinned: message.pinned,
        work: message.work.as_ref().and_then(work_row),
        appearance: message.appearance.clone().map(Into::into),
        acting: message.acting.clone(),
        share: message.share.clone(),
        attempts: message.attempts,
        sending: false,
    }
}

/// An agent's record of its work, as the reply card draws it.
fn work_row(work: &crate::record::Work) -> Option<AgentWork> {
    let transition = work.state.as_ref()?;
    let mut trace: Vec<AgentStep> = work
        .trace
        .iter()
        .filter_map(|step| {
            Some(AgentStep {
                id: Identified::id(step).to_string(),
                sequence: step.sequence,
                resource: step.resource.0.clone(),
                label: step.label.clone(),
                outcome: step.outcome.clone()?,
            })
        })
        .collect();
    trace.sort_by(|a, b| a.sequence.cmp(&b.sequence).then_with(|| a.id.cmp(&b.id)));
    Some(AgentWork {
        stream: work
            .stream
            .as_ref()
            .map(crate::record::ProviderStream::output),
        // Older records stored terminal provider output beside a stale Taken
        // claim. Project the outcome without rewriting their operation trace.
        state: work
            .stream
            .as_ref()
            .and_then(|stream| stream.finish)
            .map_or_else(
                || transition.state.clone(),
                |outcome| crate::model::AgentWorkState::ProviderFinished { outcome },
            ),
        since: transition.since,
        failure_detail: work.failure_detail.clone(),
        trace,
    })
}

/// The rich text of the draft stored under `key` in the person's drafts
/// record ([`crate::drafts::ChatDrafts`]), when it has one.
#[must_use]
pub fn draft(doc: &AutoCommit, key: &str) -> Option<ObjId> {
    let Ok(Some((Value::Object(ObjType::Map), drafts))) = doc.get(ROOT, "drafts") else {
        return None;
    };
    match doc.get(&drafts, key) {
        Ok(Some((Value::Object(ObjType::Map), draft))) => Some(draft),
        _ => None,
    }
}

#[must_use]
pub fn draft_blocks(doc: &AutoCommit, key: &str) -> Vec<ui::widget::BlockContent> {
    draft(doc, key)
        .and_then(|root| tilo_rich_text::blocks_under(doc, &root).ok())
        .unwrap_or_default()
}

/// The draft's plain text, a line per paragraph.
#[must_use]
pub fn draft_text(doc: &AutoCommit, key: &str) -> String {
    crate::body::plain_text(&draft_blocks(doc, key))
}

/// Whether the draft for `key` exists, as one rich text.
#[must_use]
pub fn has_draft(doc: &AutoCommit, key: &str) -> bool {
    draft(doc, key).is_some()
}
