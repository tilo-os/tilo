//! What people write as comments lives in records they own.
//!
//! A Commenter (a person who joined a public room, a Contributor agent) may
//! change no shared document but their own comment records
//! (`collab_types::comment`). So what a comment-only participant writes, in
//! any channel, and what anyone writes in a feedback channel is a comment
//! record of its author's device, discoverable through the room's index:
//!
//! - a report is a comment on the channel's record ([`channel_record`]);
//! - a reply is a comment on the message it answers;
//! - a person's reactions to a message are one record of theirs on that
//!   message ([`REACTIONS_LOCAL`]), a map of emoji to whether they react.
//!
//! The conversation itself — channels, their feedback flag, a report's
//! status — stays in chat's conversation document, which Editors write.
//!
//! Chat joins these records through [`JoinedRecords`]: its own are kept and
//! published, everybody else's are followed. Where no collaboration has told
//! Chat which device it is (a standalone or test run), nothing can be a comment
//! record and messages go to the conversation document as everywhere else.

use std::collections::BTreeMap;

use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
use serde::{Deserialize, Serialize};
use ui::record::{JoinedRecords, RecordCheckout, RecordId, RecordKind, RecordStore};

use crate::model::{Attachment, Message, Reaction};

/// The conversation every member's Chat opens in a room. A record id names a
/// document within one room, so the same id in every room is one
/// conversation per room, never one across rooms.
pub const CONVERSATION_RECORD: &str = tilo_reference::CHAT_CONVERSATION_RECORD;
/// Record kind of the conversation document.
pub const CONVERSATION_KIND: &str = "chat.conversation";
/// Record kind of a message someone wrote in a feedback channel.
pub const MESSAGE_KIND: &str = "chat.message";
/// Record kind of one person's reactions to one message.
pub const REACTIONS_KIND: &str = "chat.reactions";
/// The local name of a person's reactions record on a message.
pub const REACTIONS_LOCAL: &str = "reactions";

const MESSAGE_KEY: &str = "message";
const WITHDRAWN_KEY: &str = "withdrawn";
const BODY_KEY: &str = "rich";
const USER_KEY: &str = "user";
const REACTIONS_KEY: &str = "reactions";

pub fn conversation() -> RecordCheckout {
    RecordCheckout::new(
        RecordId::parse(CONVERSATION_RECORD).unwrap_or_else(RecordId::minted),
        RecordKind::new(CONVERSATION_KIND),
    )
}

/// The record a feedback channel's reports are comments on.
pub fn channel_record(channel: &str) -> Option<RecordId> {
    RecordId::parse(&tilo_reference::chat_channel_record(channel))
}

/// The record a message in the conversation document is addressed by, so a
/// reaction on it can be a comment record too.
fn conversation_message_record(message: &str) -> Option<RecordId> {
    RecordId::parse(&tilo_reference::chat_conversation_message_record(message))
}

/// A message as its author's record holds it: one authored value, like a
/// message row's reference, so a merge never pairs one text with another's
/// channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommentMessage {
    pub channel: String,
    pub id: String,
    pub author: String,
    pub ts: i64,
    pub text: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub mentions: Vec<crate::mention::Mention>,
    #[serde(default)]
    pub reference: Option<tilo_reference::ResourceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<tilo_reference::ResourceRef>,
    #[serde(default)]
    pub attachment: Option<Attachment>,
}

/// The feedback channel's records on this peer.
pub struct Comments {
    joined: JoinedRecords,
    /// The room's index as last told, so a new feedback channel is followed
    /// without waiting for the index to change again.
    index: Option<AutoCommit>,
    /// Messages found in joined records, by message id → its record.
    records: BTreeMap<String, RecordId>,
}

impl Comments {
    pub fn new(store: RecordStore) -> Self {
        Self {
            joined: JoinedRecords::new(store),
            index: None,
            records: BTreeMap::new(),
        }
    }

    pub fn joined(&self) -> &JoinedRecords {
        &self.joined
    }

    /// Whether this peer can write comment records: collaboration has told
    /// it which device it is.
    pub fn available(&self) -> bool {
        self.joined.comment_author().is_some()
    }

    /// Remember the room's index and follow what it lists on `channels`
    /// and on the conversation's `messages`.
    pub fn index_changed(&mut self, index: &AutoCommit, channels: &[String], messages: &[String]) {
        self.index = Some(index.clone());
        self.follow(channels, messages);
    }

    /// Join the comment records on `channels` and on the conversation
    /// document's `messages` (posts, replies, reactions), and the replies and
    /// reactions on those in turn. Own records stay joined whatever the index
    /// says: the index delta that brings them in may not have been sent yet.
    pub fn follow(&mut self, channels: &[String], messages: &[String]) {
        let Some(index) = &self.index else {
            return;
        };
        let mut wanted: Vec<RecordCheckout> = self
            .joined
            .joined()
            .into_iter()
            .filter(|c| self.joined.may_write(&c.id))
            .collect();
        let mut parents: std::collections::HashSet<RecordId> = channels
            .iter()
            .filter_map(|c| channel_record(c))
            .chain(
                messages
                    .iter()
                    .filter_map(|m| conversation_message_record(m)),
            )
            .collect();
        // One pass over the index, then threads resolved in memory: a reply
        // is followed once the message it answers is.
        let all = ui::record::room::comments_where(index, |_| true);
        loop {
            let mut grew = false;
            for checkout in &all {
                let Some(parent) = checkout.id.comment_parent() else {
                    continue;
                };
                if !parents.contains(&parent) || wanted.iter().any(|w| w.id == checkout.id) {
                    continue;
                }
                if checkout.kind.as_str() == MESSAGE_KIND {
                    parents.insert(checkout.id.clone());
                }
                wanted.push(checkout.clone());
                grew = true;
            }
            if !grew || wanted.len() > ui::record::MAX_JOINED_RECORDS {
                break;
            }
        }
        wanted.truncate(ui::record::MAX_JOINED_RECORDS);
        let _ = self.joined.join(&wanted);
        self.reindex();
    }

    fn reindex(&mut self) {
        self.records.clear();
        for checkout in self.joined.joined() {
            if checkout.kind.as_str() != MESSAGE_KIND {
                continue;
            }
            if let Some(message) = self.read_message(&checkout.id) {
                self.records.insert(message.id, checkout.id);
            }
        }
    }

    fn read_message(&self, id: &RecordId) -> Option<CommentMessage> {
        self.joined
            .read(id, |doc| read_str(doc, MESSAGE_KEY))
            .flatten()
            .and_then(|raw| serde_json::from_str(&raw).ok())
    }

    /// Whether `message` is one of the joined comment messages.
    pub fn holds(&self, message: &str) -> bool {
        self.records.contains_key(message)
    }

    /// The messages in `channel` joined records hold, with the reactions
    /// people's own reaction records carry. Replies are included; their
    /// `parent` threads them.
    pub fn messages(&mut self, channel: &str) -> Vec<Message> {
        self.reindex();
        let mut out = Vec::new();
        for record in self.records.values() {
            let Some(comment) = self.read_message(record) else {
                continue;
            };
            if comment.channel != channel {
                continue;
            }
            let withdrawn = self
                .joined
                .read(record, |doc| read_bool(doc, WITHDRAWN_KEY))
                .unwrap_or(false);
            out.push(Message {
                reactions: self.reactions_on(record),
                id: comment.id,
                channel: comment.channel,
                author: comment.author,
                ts: comment.ts,
                text: comment.text,
                body: self.joined.read(record, read_body).unwrap_or_default(),
                edited: false,
                history: Vec::new(),
                parent: comment.parent,
                mentions: comment.mentions,
                reference: comment.reference,
                also: comment.also,
                attachment: comment.attachment,
                attachment_withdrawn: withdrawn,
                pinned: false,
                work: None,
                report: None,
                approved: None,
            });
        }
        out.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// Reactions on a conversation-document message, from people's records.
    pub fn reactions_on_conversation_message(&self, message: &str) -> Vec<Reaction> {
        conversation_message_record(message).map_or_else(Vec::new, |r| self.reactions_on(&r))
    }

    fn reactions_on(&self, target: &RecordId) -> Vec<Reaction> {
        let mut out: Vec<Reaction> = Vec::new();
        for checkout in self.joined.joined() {
            if checkout.kind.as_str() != REACTIONS_KIND
                || checkout.id.comment_parent().as_ref() != Some(target)
            {
                continue;
            }
            let Some((user, emoji)) = self.joined.read(&checkout.id, |doc| {
                (read_str(doc, USER_KEY), read_reactions(doc))
            }) else {
                continue;
            };
            let Some(user) = user else {
                continue;
            };
            for e in emoji {
                match out.iter_mut().find(|r| r.emoji == e) {
                    Some(reaction) if !reaction.users.contains(&user) => {
                        reaction.users.push(user.clone());
                    }
                    Some(_) => {}
                    None => out.push(Reaction {
                        emoji: e,
                        users: vec![user.clone()],
                    }),
                }
            }
        }
        out
    }

    fn target(&self, message: &str) -> Option<RecordId> {
        self.records
            .get(message)
            .cloned()
            .or_else(|| conversation_message_record(message))
    }

    /// Post `message` as this device's own comment record: a report on its
    /// channel, or a reply on the message it answers.
    ///
    /// # Errors
    ///
    /// Why the record could not be written, in words for the reader.
    pub fn post(
        &mut self,
        message: &CommentMessage,
        body: &[qdoc_rich_text::ClipBlock],
    ) -> Result<(), String> {
        let parent = match &message.parent {
            Some(parent) => self.target(parent),
            None => channel_record(&message.channel),
        }
        .ok_or("this conversation cannot be commented on")?;
        let id = self
            .joined
            .comment_record(&message.id, &parent)
            .ok_or("not connected to the room yet")?;
        let json = serde_json::to_string(message).map_err(|e| e.to_string())?;
        let checkout = RecordCheckout::new(id, RecordKind::new(MESSAGE_KIND));
        self.joined
            .create(&checkout, |doc| {
                doc.put(ROOT, MESSAGE_KEY, json.as_str())
                    .map_err(|e| e.to_string())?;
                write_body(doc, body)
            })
            .map_err(|e| e.to_string())?;
        self.reindex();
        Ok(())
    }

    /// Set whether `user` reacts to `message` with `emoji`, in their own
    /// reactions record on it.
    ///
    /// # Errors
    ///
    /// Why the record could not be written.
    pub fn react(
        &mut self,
        message: &str,
        emoji: &str,
        user: &str,
        on: bool,
    ) -> Result<(), String> {
        let target = self
            .target(message)
            .ok_or("this message cannot be reacted to")?;
        let id = self
            .joined
            .comment_record(REACTIONS_LOCAL, &target)
            .ok_or("not connected to the room yet")?;
        if !self.joined.contains(&id) {
            let checkout = RecordCheckout::new(id.clone(), RecordKind::new(REACTIONS_KIND));
            let user = user.to_string();
            self.joined
                .create(&checkout, |doc| {
                    doc.put(ROOT, USER_KEY, user.as_str())
                        .and_then(|()| {
                            doc.put_object(ROOT, REACTIONS_KEY, automerge::ObjType::Map)
                                .map(|_| ())
                        })
                        .map_err(|e| e.to_string())
                })
                .map_err(|e| e.to_string())?;
        }
        self.joined
            .with(&id, |doc| {
                let map = match doc.get(ROOT, REACTIONS_KEY) {
                    Ok(Some((Value::Object(_), map))) => map,
                    _ => doc
                        .put_object(ROOT, REACTIONS_KEY, automerge::ObjType::Map)
                        .ok()?,
                };
                doc.put(&map, emoji, on).ok()
            })
            .flatten()
            .ok_or_else(|| "the reaction could not be written".to_string())
    }

    /// Withdraw the record attached to one of this device's messages.
    ///
    /// # Errors
    ///
    /// When the message is not this device's.
    pub fn withdraw(&mut self, message: &str) -> Result<(), String> {
        let record = self.records.get(message).ok_or("not a comment")?.clone();
        self.joined
            .with(&record, |doc| doc.put(ROOT, WITHDRAWN_KEY, true).ok())
            .flatten()
            .ok_or_else(|| "only its author may withdraw it".to_string())
    }
}

/// The message a comment record holds, as its author wrote it.
pub fn message_of(doc: &AutoCommit) -> Option<CommentMessage> {
    read_str(doc, MESSAGE_KEY).and_then(|raw| serde_json::from_str(&raw).ok())
}

/// The id of the message a comment record holds.
pub fn message_id_of(doc: &AutoCommit) -> Option<String> {
    message_of(doc).map(|message| message.id)
}

/// The text of the message a comment record holds.
pub fn message_text_of(doc: &AutoCommit) -> Option<String> {
    message_of(doc).map(|message| message.text)
}

/// Whether a reactions record says its author reacts with `emoji`.
pub fn reacts_with(doc: &AutoCommit, emoji: &str) -> bool {
    read_reactions(doc).iter().any(|e| e == emoji)
}

/// The body as written, as rich text under the record's `rich` map, as a
/// conversation-document message keeps it; nothing for an empty body (the
/// plain text is the whole message).
fn write_body(doc: &mut AutoCommit, body: &[qdoc_rich_text::ClipBlock]) -> Result<(), String> {
    if body.is_empty() {
        return Ok(());
    }
    let root = doc
        .put_object(ROOT, BODY_KEY, automerge::ObjType::Map)
        .map_err(|e| e.to_string())?;
    qdoc_rich_text::RichText::initialize_under(doc, &root).map_err(|e| e.to_string())?;
    tilo_rich_text::fill_clips_under(doc, &root, body).map_err(|e| e.to_string())
}

/// The body as written, when the message has one.
fn read_body(doc: &AutoCommit) -> Vec<ui::widget::BlockContent> {
    match doc.get(ROOT, BODY_KEY) {
        Ok(Some((Value::Object(_), root))) => {
            tilo_rich_text::blocks_under(doc, &root).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

fn read_str(doc: &AutoCommit, key: &str) -> Option<String> {
    match doc.get(ROOT, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Str(s) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn read_bool(doc: &AutoCommit, key: &str) -> bool {
    matches!(doc.get(ROOT, key), Ok(Some((Value::Scalar(v), _))) if matches!(v.as_ref(), ScalarValue::Boolean(true)))
}

fn read_reactions(doc: &AutoCommit) -> Vec<String> {
    let Ok(Some((Value::Object(_), map))) = doc.get(ROOT, REACTIONS_KEY) else {
        return Vec::new();
    };
    doc.keys(&map)
        .filter(|emoji| {
            matches!(doc.get(&map, emoji.as_str()), Ok(Some((Value::Scalar(v), _))) if matches!(v.as_ref(), ScalarValue::Boolean(true)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions;
    use crate::doc::ChatDoc;
    use crate::feedback::{VOTE_EMOJI, vote_count};
    use crate::model::ChannelKind;

    const ME: [u8; 32] = [7; 32];

    /// A Chat whose comments are backed by a real store, as if the host had
    /// told it its device key.
    fn app_with_identity(
        temp: &tempfile::TempDir,
    ) -> (crate::ChatApp, AutoCommit, ui::record::RecordOwners) {
        let mut owners = ui::record::RecordOwners::new(temp.path().to_path_buf());
        let store = owners
            .open("so.tilo.chat", "00000000-0000-0000-0000-0000000000c4")
            .unwrap();
        let mut app = crate::ChatApp::new_ephemeral();
        app.comments = Comments::new(store);
        app.comments.joined().set_comment_author(Some(ME));
        let mut doc = AutoCommit::new();
        ChatDoc::seed_if_empty(&mut doc);
        app.perform_action(
            &mut doc,
            &actions::create_channel_action("ws_tilo", "", "c_fb", "feedback", ChannelKind::Text),
        );
        app.perform_action(
            &mut doc,
            &actions::set_channel_feedback_action("c_fb", true),
        );
        app.active_ws = "ws_tilo".into();
        app.active_channel = "c_fb".into();
        (app, doc, owners)
    }

    #[test]
    fn in_a_feedback_channel_a_post_and_a_vote_are_the_authors_own_records() {
        let temp = tempfile::tempdir().unwrap();
        let (mut app, mut doc, _owners) = app_with_identity(&temp);
        let before = doc.get_heads();
        app.perform_action(
            &mut doc,
            &actions::send_message_action(
                "c_fb",
                "m_report",
                "u_me",
                1,
                "Send does nothing",
                None,
                None,
                Vec::new(),
            ),
        );
        app.perform_action(
            &mut doc,
            &actions::vote_action("c_fb", "m_report", "u_me", true),
        );
        assert_eq!(
            doc.get_heads(),
            before,
            "neither the report nor the vote touches the conversation document"
        );
        assert!(ChatDoc::list_messages(&doc, "c_fb").is_empty());

        let messages = app.channel_messages(&doc, "c_fb");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "Send does nothing");
        assert_eq!(vote_count(&messages[0]), 1);

        // Both records are this device's, so the host would join and publish
        // them and bring them into the room's index.
        let joined = app.comments.joined().joined();
        assert_eq!(joined.len(), 2);
        assert!(joined.iter().all(|c| c.id.comment_author() == Some(ME)));
        let report = joined
            .iter()
            .find(|c| c.kind.as_str() == MESSAGE_KIND)
            .unwrap();
        assert_eq!(
            report.id.comment_parent(),
            channel_record("c_fb"),
            "a report is a comment on its channel"
        );

        // Taking the vote back is an edit of the same record.
        app.perform_action(
            &mut doc,
            &actions::vote_action("c_fb", "m_report", "u_me", false),
        );
        assert_eq!(vote_count(&app.channel_messages(&doc, "c_fb")[0]), 0);
        assert_eq!(app.comments.joined().joined().len(), 2);

        // An Editor's status on it lives in the conversation document.
        app.perform_action(
            &mut doc,
            &actions::set_report_status_action(
                "c_fb",
                "m_report",
                crate::feedback::ReportStatus::Planned,
                "u_d417",
                2,
            ),
        );
        assert_eq!(
            crate::feedback::status_of(&app.channel_messages(&doc, "c_fb")[0]),
            crate::feedback::ReportStatus::Planned
        );
        let _ = VOTE_EMOJI;
    }

    #[test]
    fn outside_a_feedback_channel_a_message_is_the_conversations() {
        let temp = tempfile::tempdir().unwrap();
        let (mut app, mut doc, _owners) = app_with_identity(&temp);
        app.perform_action(
            &mut doc,
            &actions::send_message_action(
                "c_kernel",
                "m_plain",
                "u_me",
                1,
                "hello",
                None,
                None,
                Vec::new(),
            ),
        );
        assert_eq!(ChatDoc::list_messages(&doc, "c_kernel").len(), 1);
        assert!(app.comments.joined().is_empty());
    }

    #[test]
    fn the_rooms_index_decides_whose_comments_are_followed() {
        let temp = tempfile::tempdir().unwrap();
        let (mut app, _doc, _owners) = app_with_identity(&temp);
        let theirs = ui::record::RecordCheckout::new(
            ui::record::RecordId::for_comment(
                [9; 32],
                "m_theirs",
                &channel_record("c_fb").unwrap(),
            )
            .unwrap(),
            ui::record::RecordKind::new(MESSAGE_KIND),
        );
        let mut index = AutoCommit::new();
        assert!(ui::record::room::reference(
            &mut index,
            &theirs.id,
            &theirs.kind
        ));
        app.comments
            .index_changed(&index, &["c_fb".to_string()], &[]);
        let joined = app.comments.joined();
        assert!(joined.contains(&theirs.id));
        assert!(!joined.may_write(&theirs.id), "followed, never written");
        assert!(joined.with(&theirs.id, |_| ()).is_none());
    }
}
