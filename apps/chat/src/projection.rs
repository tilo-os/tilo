//! The chat record as the window and the agents read it: workspaces,
//! channels and messages in the shapes [`crate::model`] draws, and the
//! composers' drafts, which are rich text in the document.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::tilo_resource::{Id, Identified, Resource};

use crate::model::{
    AgentStep, AgentWork, Category, Channel, EditRecord, Message, Reaction, User, Workspace,
};
use crate::record::Chat;

/// The published id of the item `id` of the record's `field`
/// (`channels`, `messages`, …): what a caller addresses its actions to.
#[must_use]
pub fn item_id(field: &str, id: &str) -> String {
    ui::tilo_resource::items::item_resource_id::<Chat>(field, &Id::from_raw(id))
}

/// Whether `doc` holds a started chat: a record with a workspace.
#[must_use]
pub fn is_started(doc: &AutoCommit) -> bool {
    Chat::read(doc).is_ok_and(|chat| !chat.workspaces.is_empty())
}

/// The chat record in `doc`; an empty one when `doc` holds none.
#[must_use]
pub fn read(doc: &AutoCommit) -> Chat {
    Chat::read(doc).unwrap_or_default()
}

impl Chat {
    /// The workspaces, in rail order.
    #[must_use]
    pub fn workspace_rows(&self) -> Vec<Workspace> {
        self.workspaces
            .iter()
            .enumerate()
            .map(|(order, (id, workspace))| Workspace {
                id: id.to_string(),
                name: workspace.name.clone(),
                glyph: workspace.glyph.clone(),
                color_idx: usize::try_from(workspace.color).unwrap_or_default(),
                order: u32::try_from(order).unwrap_or(u32::MAX),
            })
            .collect()
    }

    /// The categories, in sidebar order.
    #[must_use]
    pub fn category_rows(&self) -> Vec<Category> {
        self.categories
            .iter()
            .enumerate()
            .map(|(order, (id, category))| Category {
                id: id.to_string(),
                workspace: category.workspace.to_string(),
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
                workspace: channel.workspace.to_string(),
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

    /// The messages of `channel`, oldest first.
    #[must_use]
    pub fn message_rows(&self, channel: &str) -> Vec<Message> {
        self.messages
            .iter()
            .filter(|(_, message)| message.channel.as_str() == channel)
            .map(|(id, message)| message_row(id, message))
            .collect()
    }

    /// The message `id`, in whichever channel it is.
    #[must_use]
    pub fn message_row(&self, id: &str) -> Option<Message> {
        let id = Id::from_raw(id);
        self.messages
            .get(&id)
            .map(|message| message_row(&id, message))
    }

    /// How many top-level messages of `channel` `user` has seen.
    #[must_use]
    pub fn read_mark(&self, channel: &str, user: &str) -> i64 {
        self.read
            .get(user)
            .and_then(|marks| marks.get(channel))
            .copied()
            .unwrap_or_default()
    }

    /// The top-level messages of `channel` `user` has not seen.
    #[must_use]
    pub fn unread(&self, channel: &str, user: &str) -> u32 {
        let total = i64::try_from(self.top_level(&Id::from_raw(channel))).unwrap_or(i64::MAX);
        u32::try_from((total - self.read_mark(channel, user)).max(0)).unwrap_or(u32::MAX)
    }
}

/// `message` as the timeline draws it.
pub(crate) fn message_row(id: &Id, message: &crate::record::Message) -> Message {
    let mut by_emoji: std::collections::BTreeMap<&str, Vec<String>> =
        std::collections::BTreeMap::new();
    for reacted in message.reactions.iter() {
        by_emoji
            .entry(reacted.emoji.as_str())
            .or_default()
            .push(reacted.user.clone());
    }
    let reactions: Vec<Reaction> = by_emoji
        .into_iter()
        .map(|(emoji, mut users)| {
            users.sort();
            Reaction {
                emoji: emoji.to_owned(),
                users,
            }
        })
        .collect();
    Message {
        id: id.to_string(),
        channel: message.channel.to_string(),
        author: message.author.0.clone(),
        ts: message.ts,
        text: message.text.clone(),
        body: message
            .body
            .0
            .iter()
            .map(tilo_rich_text::content_of_clip)
            .collect(),
        edited: message.edited,
        history: message
            .history
            .0
            .iter()
            .map(|revision| EditRecord {
                ts: revision.ts,
                text: revision.text.clone(),
            })
            .collect(),
        parent: message.parent.as_ref().map(ToString::to_string),
        reactions,
        mentions: message.mentions.0.clone(),
        reference: message.reference.0.clone(),
        source_provider: message
            .source_provider
            .as_ref()
            .map(|provider| provider.0.clone()),
        attachment: message.attachment.0.clone(),
        pinned: message.pinned,
        work: message.work.as_ref().and_then(work_row),
        appearance: message.appearance.0.clone(),
    }
}

/// An agent's record of its work, as the reply card draws it.
fn work_row(work: &crate::record::Work) -> Option<AgentWork> {
    let transition = work.state.0.as_ref()?;
    let mut trace: Vec<AgentStep> = work
        .trace
        .iter()
        .filter_map(|step| {
            Some(AgentStep {
                id: Identified::id(step).to_string(),
                sequence: step.sequence,
                resource: step.resource.0.clone(),
                label: step.label.clone(),
                outcome: step.outcome.0.clone()?,
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
            .and_then(|stream| stream.finish.0)
            .map_or_else(
                || transition.state.clone(),
                |outcome| crate::model::AgentWorkState::ProviderFinished { outcome },
            ),
        since: transition.since,
        reasoning: work.reasoning.0.clone(),
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

/// The draft for `key` as written.
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
