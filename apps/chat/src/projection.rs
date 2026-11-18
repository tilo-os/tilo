//! The chat resource as the window and the agents read it: the workspace's
//! channels and messages in the shapes [`crate::model`] draws, and the
//! composers' drafts, which are rich text in the document.

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use tilo_actions::policy::MemberId;
use ui::tilo_resource::{Id, Identified, Resource};

use crate::chat::{Chat, Conversation};
use crate::model::{AgentStep, AgentWork, Category, Channel, EditEntry, Message, User};

/// What the workspace's member list calls each of its people and agents.
pub use chat_model::chat::PeopleNames;

/// The published id of the item `id` of the resource's `field`
/// (`channels`, `messages`, …): what a caller addresses its actions to.
#[must_use]
pub fn item_id(field: &str, id: &str) -> String {
    ui::tilo_resource::items::item_resource_id::<Chat>(field, &Id::from_raw(id))
}

/// The chat resource in `doc`; an empty one when `doc` holds none.
#[must_use]
pub fn read(doc: &AutoCommit) -> Chat {
    Chat::read(doc).unwrap_or_default()
}

/// The chat resource's rows as the window and the agents read them: the shapes
/// [`crate::model`] draws, derived from the resource every render.
///
/// The resource itself is [`chat_model`]'s; its rows are the app's, because they
/// are what the window shows.
pub trait ChatProjection {
    fn category_rows(&self) -> Vec<Category>;
    fn channel_rows(&self) -> Vec<Channel>;
    fn conversation_rows(
        &self,
        me: &MemberId,
        messages: &crate::messages::Messages,
        people: &PeopleNames,
    ) -> Vec<Channel>;
    fn rows_from_parts(
        &self,
        parts: &[(u64, std::rc::Rc<Vec<Message>>)],
        messages: &crate::messages::Messages,
        channel: &str,
    ) -> Vec<Message>;
    fn message_rows(&self, messages: &crate::messages::Messages, channel: &str) -> Vec<Message>;
    fn feedback_messages(&self) -> std::vec::IntoIter<(Id, crate::chat::Message)>;
    fn message_row(&self, messages: &crate::messages::Messages, id: &str) -> Option<Message>;
    fn read_mark(&self, channel: &str, user: &MemberId) -> crate::parts::ReadMark;
    fn unread(&self, messages: &crate::messages::Messages, channel: &str, user: &MemberId) -> u32;
    fn is_direct_conversation(&self, channel: &str) -> bool;
}

impl ChatProjection for Chat {
    /// The categories, in sidebar order.
    fn category_rows(&self) -> Vec<Category> {
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
    fn channel_rows(&self) -> Vec<Channel> {
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
                members: Vec::new(),
            })
            .collect();
        rows
    }

    /// Every conversation `me` has, in sidebar order: the workspace's
    /// channels, and one direct conversation with each of the workspace's
    /// other people — the same one on every device, because its id names
    /// the members ([`Chat::direct_id`]) — with the conversations the
    /// resource keeps for an agent among them.
    ///
    /// A conversation is named by what the workspace's member list calls its people and agents
    /// (`people`), whether or not they ever opened Chat.
    fn conversation_rows(
        &self,
        me: &MemberId,
        messages: &crate::messages::Messages,
        people: &PeopleNames,
    ) -> Vec<Channel> {
        let mut rows = self.channel_rows();
        let mut direct: Vec<(String, Channel)> = Vec::new();
        let mut push = |id: &Id, members: Vec<MemberId>, recorded: Option<&Conversation>| {
            let name = self.display_names(id, Some(me), people);
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
                    members,
                },
            ));
        };
        for (id, channel) in self.channels.iter() {
            if !channel.is_channel() {
                push(id, channel.members.clone(), Some(channel));
            }
        }
        let mut listed: std::collections::HashSet<Id> = std::collections::HashSet::new();
        for person in people.keys() {
            if !person.is_person() || person == me {
                continue;
            }
            let pair = vec![me.clone(), person.clone()];
            let id = Chat::direct_id(pair.clone());
            if !self.channels.contains(&id) && listed.insert(id.clone()) {
                push(&id, pair, None);
            }
        }
        // A conversation written to by its id alone (one with an agent that a send opened) is
        // listed once it holds a message, though the resource keeps no row for it.
        for (_, message) in messages.iter().chain(messages.sending()) {
            let id = &message.channel;
            if self.channels.contains(id) || listed.contains(id) {
                continue;
            }
            let Some(members) = Chat::direct_members(id) else {
                continue;
            };
            if members.contains(me) {
                listed.insert(id.clone());
                push(id, members, None);
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

    /// The rows of `channel`, read from the parts' rows already read (`parts`,
    /// oldest first) instead of from the chain: the open channel's messages,
    /// with the feedback filed in it where it was filed and what is being sent
    /// after them. A reload reads the parts that changed and no others, and
    /// the rows of those stand here as they were read.
    fn rows_from_parts(
        &self,
        parts: &[(u64, std::rc::Rc<Vec<Message>>)],
        messages: &crate::messages::Messages,
        channel: &str,
    ) -> Vec<Message> {
        rows_from_parts(&feedback_rows(self, channel), parts, messages, channel)
    }

    /// The messages of `channel` held in `messages`, oldest first, with the
    /// feedback filed in it among them where it was filed, and the messages
    /// being sent to it after them.
    fn message_rows(&self, messages: &crate::messages::Messages, channel: &str) -> Vec<Message> {
        let rows = messages
            .iter()
            .filter(|(_, message)| message.channel.as_str() == channel)
            .map(|(id, message)| message_row(id, message))
            .collect();
        with_filed_and_sending(rows, &feedback_rows(self, channel), messages, channel)
    }

    /// Every filed report as the message its channel shows, under its
    /// message id ([`crate::feedback::message_id`]).
    fn feedback_messages(&self) -> std::vec::IntoIter<(Id, crate::chat::Message)> {
        self.feedback
            .iter()
            .filter_map(|(id, report)| {
                Some((
                    Id::from_raw(crate::feedback::message_id(id.as_str())),
                    crate::feedback::message::message(report)?,
                ))
            })
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// The message `id` held in `messages`, in whichever channel it is, or
    /// the feedback report it shows.
    fn message_row(&self, messages: &crate::messages::Messages, id: &str) -> Option<Message> {
        let key = Id::from_raw(id);
        messages
            .get(&key)
            .map(|message| message_row(&key, message))
            .or_else(|| {
                self.feedback.iter().find_map(|(report, filed)| {
                    let submission = filed.submission.as_ref()?;
                    (crate::feedback::message_id(report.as_str()) == id)
                        .then(|| feedback_rows(self, &submission.destination.channel))?
                        .into_iter()
                        .find(|row| row.id == id)
                })
            })
    }

    /// How far `user` read `channel`.
    fn read_mark(&self, channel: &str, user: &MemberId) -> crate::parts::ReadMark {
        self.read
            .get(crate::chat::member_slot(user).as_str())
            .and_then(|marks| marks.get(channel))
            .copied()
            .unwrap_or_default()
    }

    /// The top-level messages of `channel` held in `messages` that `user`
    /// has not seen. In a direct conversation, what they wrote themselves is
    /// never unread for them.
    fn unread(&self, messages: &crate::messages::Messages, channel: &str, user: &MemberId) -> u32 {
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
    fn is_direct_conversation(&self, channel: &str) -> bool {
        let id = Id::from_raw(channel);
        Chat::is_direct(&id)
            || self
                .channels
                .get(&id)
                .is_some_and(|conversation| !conversation.is_channel())
    }
}

/// The feedback filed in `channel`, as the timeline shows it.
fn feedback_rows(chat: &Chat, channel: &str) -> Vec<Message> {
    chat.feedback_messages()
        .filter(|(_, message)| message.channel.as_str() == channel)
        .map(|(id, message)| message_row(&id, &message))
        .collect()
}

/// One message as the timeline's row for it.
#[must_use]
pub fn row_of(id: &Id, message: &crate::chat::Message) -> Message {
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

pub(crate) fn message_row(id: &Id, message: &crate::chat::Message) -> Message {
    let reactions = message.reactions.groups();
    Message {
        id: id.to_string(),
        channel: message.channel.to_string(),
        author: message.author.clone(),
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
            .map(|revision| EditEntry {
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
        // A message moved from another chat shows as its author's there.
        appearance: message
            .appearance
            .clone()
            .map(Into::into)
            .or_else(|| message.origin.as_ref().map(origin_appearance)),
        acting: message.acting.clone(),
        author_name: message.author_name.clone(),
        share: message.share.clone(),
        question: message.question.clone(),
        cause: message.cause.clone(),
        view: message.view.clone(),
        attempts: message.attempts,
        sending: false,
    }
}

/// How a message moved from another chat shows its author: by their name,
/// in the tool it was written in (`Jane Doe in Slack`).
fn origin_appearance(origin: &ui::tilo_resource::Origin) -> tilo_actions::Appearance {
    tilo_actions::Appearance {
        name: crate::i18n::t_args(
            "chat-origin-author",
            &[
                ("name", origin.author.clone().into()),
                ("source", origin.source.clone().into()),
            ],
        ),
        avatar: None,
        persona: None,
    }
}

/// An agent's resource of its work, as the reply card draws it.
fn work_row(work: &crate::chat::Work) -> Option<AgentWork> {
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
            .map(crate::chat::ProviderStream::output),
        // Older resources stored terminal provider output beside a stale Taken
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
/// resource ([`crate::drafts::ChatDrafts`]), when it has one.
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
pub fn draft_blocks(doc: &AutoCommit, key: &str) -> Vec<ui::rich_edit::BlockContent> {
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

/// The people and agents of the workspace, as its member list names them
/// ([`ui::tilo_resource::members`]).
#[must_use]
pub fn member_rows() -> Vec<User> {
    ui::tilo_resource::members::named()
        .into_iter()
        .map(|(id, name)| User {
            initials: crate::identity::initials_for(&name),
            is_agent: !id.is_person(),
            status: crate::model::Presence::Online,
            id,
            name,
        })
        .collect()
}
