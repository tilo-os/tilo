//! The chat record as the window and the agents read it: the room's
//! channels and messages in the shapes [`crate::model`] draws, and the
//! composers' drafts, which are rich text in the document.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::tilo_resource::{Id, Identified, ModelError, Resource};

use crate::model::{AgentStep, AgentWork, Category, Channel, EditRecord, Message, User};
use crate::record::Chat;

/// The published id of the item `id` of the record's `field`
/// (`channels`, `messages`, …): what a caller addresses its actions to.
#[must_use]
pub fn item_id(field: &str, id: &str) -> String {
    ui::tilo_resource::items::item_resource_id::<Chat>(field, &Id::from_raw(id))
}

/// Whether `doc` holds a started chat: a record with a channel or a person.
#[must_use]
pub fn is_started(doc: &AutoCommit) -> bool {
    Chat::read(doc).is_ok_and(|chat| !chat.channels.is_empty() || !chat.people.is_empty())
}

/// The chat record in `doc`, or why it does not read as one. A document that
/// does not read is not an empty chat: whatever acts on it — a projection the
/// window shows, a write derived from one — takes this result and not a
/// default `Chat`.
///
/// # Errors
/// Returns [`ModelError`] when `doc` does not hold a chat record.
#[must_use]
pub fn try_read(doc: &AutoCommit) -> Result<Chat, ModelError> {
    Chat::read(doc)
}

/// The chat record in `doc`, or `None` when it does not read as one.
#[must_use]
pub fn read(doc: &AutoCommit) -> Option<Chat> {
    Chat::read(doc).ok()
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

    /// The channels, in sidebar order; unread counts are the reader's own.
    #[must_use]
    pub fn channel_rows(&self) -> Vec<Channel> {
        self.channels
            .iter()
            .filter(|(_, channel)| channel.conversation_with.is_none())
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
                conversation_with: channel
                    .conversation_with
                    .as_ref()
                    .map(|agent| agent.0.clone()),
            })
            .collect()
    }

    /// The people and agents in the room.
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
                is_agent: person.agent,
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

    /// The messages of `channel` held in `messages`, oldest first, with the
    /// feedback filed in it among them where it was filed.
    #[must_use]
    pub fn message_rows(
        &self,
        messages: &crate::messages::Messages,
        channel: &str,
    ) -> Vec<Message> {
        let mut rows: Vec<Message> = messages
            .iter()
            .filter(|(_, message)| message.channel.as_str() == channel)
            .map(|(id, message)| message_row(id, message))
            .collect();
        let filed = self.feedback_rows(channel);
        if !filed.is_empty() {
            for report in filed {
                let at = rows.partition_point(|row| row.ts <= report.ts);
                rows.insert(at, report);
            }
        }
        rows
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
    /// has not seen.
    #[must_use]
    pub fn unread(&self, messages: &crate::messages::Messages, channel: &str, user: &str) -> u32 {
        let unread = messages.unread(&Id::from_raw(channel), self.read_mark(channel, user));
        u32::try_from(unread).unwrap_or(u32::MAX)
    }
}

/// `message` as the timeline draws it.
#[must_use]
pub fn message_row(id: &Id, message: &crate::record::Message) -> Message {
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
        reactions,
        mentions: message.mentions.clone(),
        reference: message.reference.clone(),
        source_provider: message
            .source_provider
            .as_ref()
            .map(|provider| provider.0.clone()),
        attachment: message.attachment.clone(),
        pinned: message.pinned,
        work: message.work.as_ref().and_then(work_row),
        appearance: message.appearance.clone().map(Into::into),
        share: message.share.clone(),
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
        reasoning: work.reasoning.clone(),
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

#[cfg(test)]
mod tests {
    use super::{read, try_read};
    use automerge::{ROOT, ReadDoc, transaction::Transactable as _};

    /// A started chat: one channel, as a room's document holds it.
    fn started() -> automerge::AutoCommit {
        let mut doc = automerge::AutoCommit::new();
        crate::demo::start(&mut doc, "me", "Me", "ME").expect("a room starts");
        doc
    }

    /// `doc` with its channels no longer a keyed collection: what a document
    /// a newer or broken writer left behind reads as.
    fn corrupt(doc: &mut automerge::AutoCommit) {
        assert!(doc.get(ROOT, "channels").unwrap().is_some());
        doc.put(ROOT, "channels", "not a keyed collection")
            .expect("the field is written");
    }

    #[test]
    fn a_document_that_does_not_read_is_not_an_empty_chat() {
        let mut doc = started();
        assert!(try_read(&doc).is_ok(), "a room's document reads");
        assert!(!read(&doc).expect("a chat record").channels.is_empty());

        corrupt(&mut doc);
        let why = try_read(&doc).expect_err("a broken document does not read");
        assert!(
            why.to_string().contains("channels"),
            "the read says where it failed: {why}"
        );
        assert!(
            read(&doc).is_none(),
            "a document that does not read yields no chat, not an empty one"
        );
        assert!(
            !super::is_started(&doc),
            "a chat that cannot be read has not been shown to be started"
        );
    }
}
