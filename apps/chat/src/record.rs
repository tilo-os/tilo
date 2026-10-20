//! The chat record: the room's categories and channels, the people in the
//! room, each person's read marks, reviewed feedback and moderation. Chat
//! runs in a Tilo workspace (its room) and holds no workspaces of its own:
//! every channel of the record is the room's. The channels' messages are kept in parts beside it
//! ([`crate::parts`], [`crate::messages`]), so the record stays small
//! however long the chat runs. What someone is writing and has not sent
//! is theirs, not the room's: it is kept in their own record
//! ([`crate::drafts`]).
//!
//! Plain changes are the built-in field edits every path offers: renaming a
//! channel or a category, setting a channel's topic, archiving it, typing in
//! a draft. The methods of [`Chat`] are the operations that are more than
//! setting a field: creating and reordering channels, reporting, marking a
//! channel read.
//!
//! Who posts, reacts or reads is whoever invokes the verb ([`ActionCx::author`]),
//! never an argument; when is the action's time ([`ActionCx::now_ms`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tilo_document::ClipBlock;
use ui::automerge::ObjId;
use ui::tilo_resource::evolution::{collection_items, map_at, map_values, retype_json};
use ui::tilo_resource::{
    self, ActionCx, Atom, Collection, Id, Identified, JsonSchema, Keyed, Model, ModelError, Refusal,
};

use crate::model::{ChannelKind, Presence};

/// Longest message body, in bytes: a conversation turn, not a document.
pub const MAX_MESSAGE_TEXT_BYTES: usize = 64 * 1024;
/// Longest agent display name recorded for an agent task.
pub const MAX_AGENT_NAME_BYTES: usize = 80;
/// Largest attachment a message refers to.
pub const MAX_ATTACHMENT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ATTACHMENT_NAME_BYTES: usize = 255;
const MAX_ATTACHMENT_DIMENSION: u32 = 16_384;

/// The id of the room's text channel every chat opens with, `#chat`.
pub const CHAT_CHANNEL: &str = "c_chat";
/// The id of the room's voice channel every chat opens with, `#voice`.
pub const VOICE_CHANNEL: &str = "c_voice";

/// Bring a chat record of version `from` up to this one. Version 1 kept
/// everyone's unsent drafts in the room, where every member edited one
/// shared draft per channel; drafts are each writer's own now
/// ([`crate::drafts`]), so the room's copy is dropped. Versions 1 and 2
/// kept the messages in the record and read marks as counts; the messages
/// are kept in parts beside it now ([`crate::parts`]) and a read mark is a
/// position in them, so both are dropped: a record outgrown by its messages
/// is what parts replace, and nothing carries them over. Versions 1 to 3
/// kept workspaces of the chat's own; Chat runs in its room now, so that
/// structure is dropped and every channel and category stays
/// ([`drop_workspaces`]). Versions 1 to 4 kept a filed report's submission,
/// a triage step's update and a moderation decision's reports and
/// take-backs as one JSON string each; they are model values now, rewritten
/// in place.
///
/// # Errors
/// When `from` is not a version this chat was, or a stored value does not
/// read as its new type.
fn migrate(
    doc: &mut ui::tilo_resource::Doc,
    from: u32,
) -> Result<(), ui::tilo_resource::ModelError> {
    use ui::automerge::{ROOT, ReadDoc as _, transaction::Transactable as _};
    if !(1..=4).contains(&from) {
        return Err(ui::tilo_resource::ModelError::invalid(format!(
            "no Chat migration from {from}"
        )));
    }
    if from <= 2 {
        for dropped in ["drafts", "messages"] {
            if doc.get(ROOT, dropped)?.is_some() {
                doc.delete(ROOT, dropped)?;
            }
        }
        // Read marks start over as positions in the parts.
        let read = ui::tilo_resource::Slot::Key(&ROOT, "read");
        read.remove(doc)?;
        read.object(doc, ui::automerge::ObjType::Map)?;
    }
    if from <= 3 {
        drop_workspaces(doc)?;
    }
    for report in collection_items(doc, &ROOT, "feedback")? {
        retype_json::<Option<ui::feedback::Submission>>(doc, &report, "submission")?;
        for step in collection_items(doc, &report, "triage")? {
            retype_json::<Option<crate::feedback::TriageUpdate>>(doc, &step, "update")?;
        }
    }
    if let Some(moderation) = map_at(doc, &ROOT, "moderation")? {
        moderation_model::record::upgrade_decisions(doc, &moderation)?;
    }
    Ok(())
}

/// Drop the workspaces a chat of version 3 or older kept: they were the
/// chat's own structure, not anything a person wrote. Every channel and
/// every category stays, as the room's, without the workspace it named;
/// nothing else is added or removed. A category that two devices' starters
/// left without its workspace (the field whose absence made the user's chat
/// unopenable, `categories.cat_home_text.v.workspace: required value is
/// missing`) reads again once the field is no longer the model's.
///
/// # Errors
/// The document's read or write error.
fn drop_workspaces(doc: &mut ui::tilo_resource::Doc) -> Result<(), ui::tilo_resource::ModelError> {
    use ui::automerge::{ObjType, ROOT, ReadDoc as _, Value, transaction::Transactable as _};
    if doc.get(ROOT, "workspaces")?.is_some() {
        doc.delete(ROOT, "workspaces")?;
    }
    for collection in ["channels", "categories"] {
        let Some((Value::Object(ObjType::Map), map)) = doc.get(ROOT, collection)? else {
            continue;
        };
        let entries: Vec<ui::automerge::ObjId> = doc
            .map_range(&map, ..)
            .filter(|entry| entry.value == ui::automerge::ValueRef::Object(ObjType::Map))
            .map(|entry| entry.id())
            .collect();
        for entry in entries {
            // Every copy of the entry's value: two devices that made the
            // same item each made one, and the document keeps both.
            let values: Vec<ui::automerge::ObjId> = doc
                .get_all(&entry, "v")?
                .into_iter()
                .filter(|(value, _)| matches!(value, Value::Object(ObjType::Map)))
                .map(|(_, id)| id)
                .collect();
            for value in values {
                if doc.get(&value, "workspace")?.is_some() {
                    doc.delete(&value, "workspace")?;
                }
            }
        }
    }
    Ok(())
}

/// Bring a part of the chat's messages of version 1 up to this one. Version
/// 1 kept a message's paragraphs, earlier texts, mentions, reference,
/// attachment and shown name, and an agent's record of its work, as one
/// JSON string each; they are model values now, rewritten in place.
///
/// # Errors
/// When `from` is not a version a part was, or a stored value does not read
/// as its new type.
pub(crate) fn upgrade_part(doc: &mut ui::tilo_resource::Doc, from: u32) -> Result<(), ModelError> {
    if from != 1 {
        return Err(ModelError::invalid(format!(
            "no chat part migration from {from}"
        )));
    }
    for message in collection_items(doc, &ui::automerge::ROOT, "messages")? {
        upgrade_message(doc, &message)?;
    }
    Ok(())
}

/// [`upgrade_part`] of one message stored at `message`.
fn upgrade_message(doc: &mut ui::tilo_resource::Doc, message: &ObjId) -> Result<(), ModelError> {
    use crate::model::{AgentReasoning, AgentStepOutcome, Attachment};
    retype_json::<Vec<ClipBlock>>(doc, message, "body")?;
    retype_json::<Vec<Revision>>(doc, message, "history")?;
    retype_json::<Vec<crate::mention::Mention>>(doc, message, "mentions")?;
    retype_json::<Option<tilo_reference::ResourceRef>>(doc, message, "reference")?;
    retype_json::<Option<Attachment>>(doc, message, "attachment")?;
    retype_json::<Option<ShownAs>>(doc, message, "appearance")?;
    if let Some(work) = map_at(doc, message, "work")? {
        retype_json::<Option<Transition>>(doc, &work, "state")?;
        retype_json::<Option<AgentReasoning>>(doc, &work, "reasoning")?;
        if let Some(stream) = map_at(doc, &work, "stream")? {
            retype_json::<Option<crate::agent_stream::AgentFinish>>(doc, &stream, "finish")?;
        }
        for (_, step) in map_values(doc, &work, "trace")? {
            retype_json::<Option<AgentStepOutcome>>(doc, &step, "outcome")?;
        }
    }
    Ok(())
}

/// Give a room opened for the first time, with no channels at all, its
/// first two: `#chat` to talk in and `#voice` to call in, under fixed ids
/// and with the same values, so every device that opens the room first
/// makes the very same two, and their copies are one item each. A room
/// with any channel, even one that does not read, gets nothing.
fn open_room(channels: &mut Collection<Channel>) {
    if !channels.is_empty() || !channels.quarantined().is_empty() {
        return;
    }
    for (id, name, kind) in [
        (CHAT_CHANNEL, "chat", ChannelKind::Text),
        (VOICE_CHANNEL, "voice", ChannelKind::Voice),
    ] {
        channels.push(
            Id::from_raw(id),
            Channel {
                name: name.into(),
                kind,
                ..Channel::default()
            },
        );
    }
}

/// The room's chat. See the module docs.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Chat {
    /// The room's channel categories, in sidebar order: made, ordered and
    /// removed only by the record's actions, which keep them to those who
    /// keep the room's layout. A generic insert, move or removal would let
    /// anyone who may edit the room (a public room's visitor) rearrange it.
    #[model(readonly)]
    pub categories: Collection<Category>,
    /// The room's channels, in sidebar order within their category: made,
    /// ordered and removed only by the record's actions, for the same
    /// reason as [`Self::categories`].
    #[model(readonly)]
    pub channels: Collection<Channel>,
    /// The people and agents in the room, by their participant id.
    pub people: Keyed<Person>,
    /// The name every member shows for an agent task, as the person hosting
    /// it labelled it.
    pub agent_names: BTreeMap<String, String>,
    /// Each person's read mark per channel: how far into the chain of
    /// message parts they have read it.
    pub read: BTreeMap<String, BTreeMap<String, crate::parts::ReadMark>>,
    /// Reviewed feedback filed in a channel, under its submission's id.
    pub feedback: Collection<FeedbackReport>,
    /// Reports of people and messages, and what the room's moderators (or
    /// its host's moderation agent) decided on them.
    pub moderation: moderation_model::Moderation,
}

/// A collapsible group of the room's channels.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Category {
    /// The category's name: changed by renaming it
    /// ([`Chat::rename_category`]), which only those who keep the room's
    /// layout may.
    #[model(label, readonly)]
    pub name: String,
}

/// A text or voice channel of the room.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Channel {
    /// The category it is listed under; none for uncategorised: changed by
    /// [`Chat::set_channel_category`], which only those who keep the room's
    /// layout may.
    #[model(readonly)]
    pub category: Option<Id>,
    /// The channel's name: changed by renaming it
    /// ([`Chat::rename_channel`]), which only those who keep the room's
    /// layout may.
    #[model(label, readonly)]
    pub name: String,
    /// What the channel is about: changed by [`Chat::set_channel_topic`],
    /// which only those who keep the room's layout may.
    #[model(readonly)]
    pub topic: String,
    /// A text channel has a conversation; a voice channel has a call.
    #[model(readonly)]
    pub kind: ChannelKind,
    /// Archived channels leave the sidebar and stay in the record: changed
    /// by archiving it ([`Chat::archive_channel`]), which only those who
    /// keep the room's layout may.
    #[model(readonly)]
    pub archived: bool,
    /// Bare authenticated Agent id for a direct conversation; ordinary
    /// channels leave it empty and remain in the navigation list.
    #[model(readonly)]
    pub conversation_with: Option<Atom>,
}

/// One message of a channel.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Message {
    /// The channel it was posted in.
    #[model(readonly)]
    pub channel: Id,
    /// Who wrote it: their participant id.
    #[model(readonly)]
    pub author: Atom,
    /// When, in seconds since the epoch.
    #[model(readonly)]
    pub ts: i64,
    /// Its plain text: what search, agents and notifications read.
    #[model(label, readonly)]
    pub text: String,
    /// Its paragraphs as written, with their formatting; empty for plain text.
    #[model(readonly)]
    pub body: Vec<ClipBlock>,
    /// Whether it was changed after it was posted.
    #[model(readonly)]
    pub edited: bool,
    /// Its earlier texts, oldest first.
    #[model(readonly)]
    pub history: Vec<Revision>,
    /// The message it answers in a thread.
    #[model(readonly)]
    pub parent: Option<Id>,
    /// Who reacted with which emoji: one element per person and emoji, so
    /// two people's first reactions with one emoji both count.
    #[model(readonly)]
    pub reactions: ui::tilo_resource::Set<Reacted>,
    /// Who the text refers to, resolved when it was written.
    #[model(readonly)]
    pub mentions: Vec<crate::mention::Mention>,
    /// The thing it is about, when it was written by sharing something.
    #[model(readonly)]
    pub reference: Option<tilo_reference::ResourceRef>,
    /// The record the reference lives in, when the host resolved one.
    #[model(readonly)]
    pub source_provider: Option<Atom>,
    /// A file, an image or a video, by its content address.
    #[model(readonly)]
    pub attachment: Option<crate::model::Attachment>,
    /// Pinned messages are listed for everyone (`pin`).
    #[model(readonly)]
    pub pinned: bool,
    /// An agent's record of what it is doing with the message this one
    /// answers.
    #[model(readonly)]
    pub work: Option<Work>,
    /// The name and avatar a bot showed for it instead of its own (a bridge
    /// posting an outside person's line); the author stays the bot.
    #[model(readonly)]
    pub appearance: Option<ShownAs>,
}

/// The name and avatar a bot showed for a message instead of its own, as the
/// message keeps it: [`tilo_actions::Appearance`] as stored values.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShownAs {
    /// The shown name.
    #[model(atom)]
    pub name: String,
    /// BLAKE3 hex of an image in the room's content store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// The outside account this stands for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<ShownPersona>,
}

/// An account on another service a message was shown as coming from.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShownPersona {
    #[model(atom)]
    pub service: String,
    #[model(atom)]
    pub account: String,
}

impl From<tilo_actions::Appearance> for ShownAs {
    fn from(appearance: tilo_actions::Appearance) -> Self {
        Self {
            name: appearance.name,
            avatar: appearance.avatar,
            persona: appearance.persona.map(|persona| ShownPersona {
                service: persona.service,
                account: persona.account,
            }),
        }
    }
}

impl From<ShownAs> for tilo_actions::Appearance {
    fn from(shown: ShownAs) -> Self {
        Self {
            name: shown.name,
            avatar: shown.avatar,
            persona: shown.persona.map(|persona| tilo_actions::Persona {
                service: persona.service,
                account: persona.account,
            }),
        }
    }
}

impl Default for Message {
    fn default() -> Self {
        Self {
            channel: Id::from_raw(""),
            author: Atom::default(),
            ts: 0,
            text: String::new(),
            body: Vec::new(),
            edited: false,
            history: Vec::new(),
            parent: None,
            reactions: ui::tilo_resource::Set::new(),
            mentions: Vec::new(),
            reference: None,
            source_provider: None,
            attachment: None,
            pinned: false,
            work: None,
            appearance: None,
        }
    }
}

/// One person's reaction to a message with one emoji.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reacted {
    pub emoji: String,
    pub user: String,
}

ui::tilo_resource::json_model!(Reacted);

/// An earlier text of a message.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    pub ts: i64,
    pub text: String,
}

/// A person or agent in the room.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Person {
    /// Their participant id: what messages, reactions and read marks name.
    #[model(readonly)]
    pub id: Atom,
    /// The name they show.
    #[model(label)]
    pub name: String,
    /// Their monogram.
    pub initials: String,
    /// Their colour.
    pub color: u32,
    /// Whether they are around.
    pub status: Presence,
    /// An automated agent rather than a person.
    #[model(readonly)]
    pub agent: bool,
}

impl Identified for Person {
    fn id(&self) -> Id {
        Id::from_raw(self.id.0.clone())
    }
}

/// An agent's record of its work on a message, written by that agent.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Work {
    /// Incremental provider output; independent of the agent's public rationale.
    pub stream: Option<ProviderStream>,
    /// Where it is, and since when: one authored claim, never mixed with
    /// another's time.
    pub state: Option<Transition>,
    /// How it decided, in the decider's words.
    pub reasoning: Option<crate::model::AgentReasoning>,
    /// One row per operation it dispatched, rewritten when the app answers.
    pub trace: Keyed<Step>,
}

/// Received provider output, stored as fields and CRDT text, not a JSON snapshot.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ProviderStream {
    pub run: String,
    pub sequence: u64,
    pub reasoning: String,
    pub progress: String,
    pub finish: Option<crate::agent_stream::AgentFinish>,
    pub detail: String,
}

impl ProviderStream {
    pub(crate) fn output(&self) -> crate::agent_stream::AgentStream {
        crate::agent_stream::AgentStream {
            run: self.run.clone(),
            sequence: self.sequence,
            reasoning: self.reasoning.clone(),
            progress: self.progress.clone(),
            finish: self.finish,
            detail: self.detail.clone(),
        }
    }
}

/// One state of an agent's work and when it entered it.
#[derive(Model, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    pub state: crate::model::AgentWorkState,
    pub since: i64,
}

/// One operation an agent dispatched, keyed by the id minted when it did.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Step {
    pub id: Atom,
    pub sequence: u64,
    pub resource: Atom,
    #[model(label)]
    pub label: String,
    pub outcome: Option<crate::model::AgentStepOutcome>,
}

impl Identified for Step {
    fn id(&self) -> Id {
        Id::from_raw(self.id.0.clone())
    }

    fn order(&self, other: &Self) -> std::cmp::Ordering {
        self.sequence.cmp(&other.sequence)
    }
}

/// A reviewed feedback report filed in a channel.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct FeedbackReport {
    /// What was submitted, as the person reviewed it.
    #[model(readonly)]
    pub submission: Option<ui::feedback::Submission>,
    /// Who filed it.
    #[model(readonly)]
    pub author: Atom,
    /// When, in seconds since the epoch.
    #[model(readonly)]
    pub ts: i64,
    /// What happened to it since.
    #[model(readonly)]
    pub triage: Collection<Triage>,
}

/// One triage step of a feedback report.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Triage {
    #[model(readonly)]
    pub author: Atom,
    #[model(readonly)]
    pub ts: i64,
    #[model(readonly)]
    pub update: Option<crate::feedback::TriageUpdate>,
}

/// What an agent records of its work, as a verb takes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkUpdate {
    /// `taken`, `waiting`, `doing`, `done`, `refused`, or a `provider_` terminal outcome.
    pub state: String,
    /// The app waited on (`waiting`).
    #[serde(default)]
    pub app: Option<String>,
    /// The operation in the app's words (`doing`, `done`).
    #[serde(default)]
    pub did: Option<String>,
    /// What it is about (`doing`).
    #[serde(default)]
    pub about: Option<String>,
    /// Which path decided: `model` or `floor`.
    #[serde(default)]
    pub route: Option<String>,
    /// The decider's account of why.
    #[serde(default)]
    pub why: Option<String>,
    /// The operations dispatched so far, each by the id minted when it was.
    #[serde(default)]
    pub trace: Vec<StepUpdate>,
}

/// One dispatched operation, as an agent reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StepUpdate {
    pub id: String,
    #[serde(default)]
    pub sequence: u64,
    pub resource: String,
    pub label: String,
    /// `asked`, `applied`, `no_change` or `refused`.
    pub outcome: String,
    /// Why it was refused.
    #[serde(default)]
    pub why: Option<String>,
}

/// A shared thing's address, as `share-resource` takes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SharedReference {
    /// The publishing app's kind tag (`word_processor.block`).
    pub kind: String,
    /// Its resource id.
    pub id: String,
    /// What the chip says.
    pub label: String,
    /// The part of it, when not the whole.
    #[serde(default)]
    pub span: Option<SharedSpan>,
}

impl SharedReference {
    /// The address a message stores.
    #[must_use]
    pub fn resolved(&self) -> tilo_reference::ResourceRef {
        let span = self.span.as_ref().map(|span| tilo_reference::Span {
            start: span.start,
            end: span.end,
            unit: match span.unit.as_str() {
                "Bytes" => tilo_reference::SpanUnit::Bytes,
                _ => tilo_reference::SpanUnit::Chars,
            },
        });
        tilo_reference::ResourceRef {
            kind: self.kind.clone(),
            id: self.id.clone(),
            label: self.label.clone(),
            span,
        }
    }
}

/// A span of a shared thing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SharedSpan {
    pub start: usize,
    pub end: usize,
    /// `Chars` or `Bytes`.
    pub unit: String,
}

/// An attachment, as `post` takes it: content another action put in the
/// store, by its hash.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AttachmentUpdate {
    /// `image`, `video` or `file`.
    pub kind: String,
    pub name: String,
    /// The content hash.
    pub blob: String,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub size: u64,
}

impl AttachmentUpdate {
    /// The attachment a message stores.
    #[must_use]
    pub fn attachment(&self) -> Option<crate::model::Attachment> {
        use crate::model::Attachment;
        Some(match self.kind.as_str() {
            "image" => Attachment::Image {
                name: self.name.clone(),
                blob: self.blob.clone(),
                w: self.width,
                h: self.height,
            },
            "video" => Attachment::Video {
                name: self.name.clone(),
                blob: self.blob.clone(),
                w: self.width,
                h: self.height,
                size: self.size,
            },
            "file" => Attachment::File {
                name: self.name.clone(),
                blob: self.blob.clone(),
                size: self.size,
            },
            _ => return None,
        })
    }

    /// `attachment` as `post` takes it.
    #[must_use]
    pub fn of(attachment: &crate::model::Attachment) -> Self {
        use crate::model::Attachment;
        match attachment {
            Attachment::Image { name, blob, w, h } => Self {
                kind: "image".into(),
                name: name.clone(),
                blob: blob.clone(),
                width: *w,
                height: *h,
                size: 0,
            },
            Attachment::Video {
                name,
                blob,
                w,
                h,
                size,
            } => Self {
                kind: "video".into(),
                name: name.clone(),
                blob: blob.clone(),
                width: *w,
                height: *h,
                size: *size,
            },
            Attachment::File { name, blob, size } => Self {
                kind: "file".into(),
                name: name.clone(),
                blob: blob.clone(),
                width: 0,
                height: 0,
                size: *size,
            },
        }
    }
}

/// Describe the file a `post-file` names where it is posted: read it from
/// this device's content store, so every replica posts the same picture or
/// file, of its true size, and content that is not here is refused before
/// anyone is shown a message pointing at nothing.
pub(crate) fn prepare_file_post(
    params: &mut crate::messages::chat_part::PostFile,
) -> Result<(), String> {
    let address = params.content.trim();
    let hash = tilo_actions::content_hash(address)
        .unwrap_or(address)
        .to_owned();
    if !tilo_content::valid_hash(&hash) {
        return Err(format!(
            "{address:?} is not a content address: post what another step handed over"
        ));
    }
    let name = params.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 255 || name.contains(['/', '\\']) {
        return Err("a posted file has a short name without a slash".into());
    }
    let store = ui::tilo_resource::content::current()
        .ok_or("this window has no content store to post from")?;
    let bytes = store
        .read(&hash)
        .ok_or("the handed-over file is not on this device; hand it over again here")?;
    let meta = tilo_content::sniff(&bytes);
    let kind = if meta.mime.starts_with("image/") && meta.w > 0 && meta.h > 0 {
        "image"
    } else {
        "file"
    };
    params.content.clone_from(&hash);
    params.name.clone_from(&name);
    params.attachment = Some(AttachmentUpdate {
        kind: kind.into(),
        name,
        blob: hash,
        width: meta.w,
        height: meta.h,
        size: meta.len,
    });
    Ok(())
}

/// Who an action is by; refused without one, since a report or a read mark
/// is always someone's.
fn author(cx: &ActionCx) -> Result<String, Refusal> {
    cx.author()
        .filter(|author| !author.is_empty() && author.len() <= 4096)
        .map(str::to_owned)
        .ok_or_else(|| Refusal::new("an action in chat is always someone's"))
}

fn seconds(cx: &ActionCx) -> i64 {
    cx.now_ms().div_euclid(1000)
}

pub(crate) fn valid_attachment(attachment: &crate::model::Attachment) -> bool {
    use crate::model::Attachment;
    let valid_name = |name: &str| {
        !name.is_empty()
            && name.len() <= MAX_ATTACHMENT_NAME_BYTES
            && !name.chars().any(char::is_control)
    };
    match attachment {
        Attachment::Image { name, blob, w, h } => {
            valid_name(name)
                && tilo_content::valid_hash(blob)
                && (1..=MAX_ATTACHMENT_DIMENSION).contains(w)
                && (1..=MAX_ATTACHMENT_DIMENSION).contains(h)
        }
        Attachment::Video {
            name,
            blob,
            w,
            h,
            size,
        } => {
            valid_name(name)
                && tilo_content::valid_hash(blob)
                && (1..=MAX_ATTACHMENT_BYTES).contains(size)
                && *w <= MAX_ATTACHMENT_DIMENSION
                && *h <= MAX_ATTACHMENT_DIMENSION
        }
        Attachment::File { name, blob, size } => {
            valid_name(name)
                && tilo_content::valid_hash(blob)
                && (1..=MAX_ATTACHMENT_BYTES).contains(size)
        }
    }
}

/// A portable address, never copied content, a local path or an access grant.
pub(crate) fn valid_reference(reference: &tilo_reference::ResourceRef) -> bool {
    let field = |value: &str, max: usize| {
        !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
    };
    field(&reference.kind, 128)
        && reference
            .kind
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && field(&reference.id, 2048)
        && !reference.id.starts_with(['/', '\\'])
        && !reference.id.contains("://")
        && field(&reference.label, 512)
        && reference.span.is_none_or(|span| span.start <= span.end)
}

/// An existing typed record provider. The host resolves and authorizes it;
/// storing this address neither admits a collaborator nor grants Read.
pub(crate) fn valid_source_provider(provider: &str) -> bool {
    let Some(rest) = provider.strip_prefix("record:") else {
        return false;
    };
    let Some((app, _record)) = rest.split_once(':') else {
        return false;
    };
    provider.len() <= 2048
        && !app.is_empty()
        && !provider.chars().any(char::is_control)
        && !provider.contains("://")
        && !provider.contains('/')
        && !provider.contains('\\')
}

/// The id of `agent`'s reply to the message `summons`: the same on every
/// replica and for every record of its work, so the agent never needs to be
/// told the id of what it wrote.
#[must_use]
pub fn reply_id(summons: &Id, agent: &str) -> Id {
    use std::hash::Hasher as _;
    let mut hash = fnv::FnvHasher::default();
    hash.write(summons.as_str().as_bytes());
    hash.write(&[0]);
    hash.write(agent.as_bytes());
    Id::from_raw(format!("m_reply_{:016x}", hash.finish()))
}

/// Stable hidden conversation the room's replicas share with an
/// authenticated Agent. The id is an address only; admission still comes
/// from the live room roster.
#[must_use]
pub fn agent_conversation_channel_id(agent: &str) -> Id {
    use std::hash::Hasher as _;
    let mut hash = fnv::FnvHasher::default();
    hash.write(b"room");
    hash.write(&[0]);
    hash.write(agent.as_bytes());
    Id::from_raw(format!("c_agent_{:016x}", hash.finish()))
}

/// A new message's fields beyond its channel, author and time.
#[derive(Default)]
pub(crate) struct Posting {
    pub text: String,
    pub body: Vec<ClipBlock>,
    pub parent: Option<Id>,
    pub attachment: Option<crate::model::Attachment>,
    pub reference: Option<tilo_reference::ResourceRef>,
    pub source_provider: Option<String>,
    pub appearance: Option<tilo_actions::Appearance>,
    /// Who the text refers to, as the chat record resolved it where the
    /// message was written.
    pub mentions: Vec<crate::mention::Mention>,
}

impl Chat {
    /// Everyone who can be mentioned: the room's people and agents.
    #[must_use]
    pub fn participants(&self) -> Vec<crate::mention::Participant> {
        self.people
            .iter()
            .map(|person| crate::mention::Participant {
                id: person.id.0.clone(),
                name: person.name.clone(),
                is_agent: person.agent,
            })
            .collect()
    }

    /// Who a message refers to: the mentions marked in its paragraphs as
    /// written, or for plain text each `@Name` of a member.
    pub(crate) fn mentions(&self, text: &str, body: &[ClipBlock]) -> Vec<crate::mention::Mention> {
        if body.is_empty() {
            return crate::mention::resolve(text, &self.participants());
        }
        let blocks: Vec<_> = body.iter().map(tilo_rich_text::content_of_clip).collect();
        crate::body::mentions(&blocks, &self.participants())
    }

    /// Add `channel` as `id`.
    pub(crate) fn open_channel(&mut self, id: Id, channel: Channel) {
        self.channels.push(id, channel);
    }

    /// The person `id`, added when the room does not know them yet.
    fn person(&mut self, id: &str) -> Result<&mut Person, Refusal> {
        let key = Id::from_raw(id);
        if self.people.find(&key).is_none() {
            self.people.push(Person {
                id: Atom(id.to_owned()),
                agent: id.starts_with(tilo_actions::AGENT_TASK_PARTICIPANT_PREFIX),
                ..Person::default()
            });
        }
        self.people
            .find_mut(&key)
            .ok_or_else(|| Refusal::new("there is no such person"))
    }

    /// Start a fresh record: its creator, and the room's first channels
    /// when it has none yet ([`open_room`]).
    pub(crate) fn begin(
        &mut self,
        creator: &str,
        (name, initials, color): (&str, &str, u32),
    ) -> Result<(), Refusal> {
        let person = self.person(creator)?;
        name.clone_into(&mut person.name);
        initials.clone_into(&mut person.initials);
        person.color = color;
        person.status = Presence::Online;
        open_room(&mut self.channels);
        Ok(())
    }
}

#[ui::tilo_resource::resource(
    kind = "chat.workspace",
    label = "Chat",
    params = chat,
    search = Chat::search,
    read_content = Chat::content,
    effects = crate::effects::ChatEffect,
    item_views = crate::effects::item_views,
    version = 5,
    migrate = migrate,
)]
impl Chat {
    /// Start the chat.
    ///
    /// Its creator, and the room's first channels when it has none yet.
    #[action(housekeeping)]
    pub fn begin_chat(
        &mut self,
        cx: &mut ActionCx,
        #[param("Your name")] name: String,
        #[param("Your initials")] initials: String,
        #[param("Your colour")] color: u32,
    ) -> Result<(), Refusal> {
        let creator = author(cx)?;
        if name.len() > 128 || initials.len() > 8 {
            return Err(Refusal::new("the creator's name is too long"));
        }
        self.begin(&creator, (&name, &initials, color))
    }

    /// Introduce yourself.
    ///
    /// How you show in the room: your name, initials and colour, and that
    /// you are here.
    #[action(housekeeping)]
    pub fn introduce(
        &mut self,
        cx: &mut ActionCx,
        #[param("Your name")] name: String,
        #[param("Your initials")] initials: String,
        #[param("Your colour")] color: u32,
    ) -> Result<(), Refusal> {
        let me = author(cx)?;
        if name.len() > 128 || initials.len() > 8 {
            return Err(Refusal::new("the name is too long"));
        }
        let person = self.person(&me)?;
        person.name = name;
        person.initials = initials;
        person.color = color;
        person.status = Presence::Online;
        Ok(())
    }

    /// Name an agent.
    ///
    /// The name every member shows for an agent task.
    #[action]
    pub fn name_agent(&mut self, participant: String, name: String) -> Result<(), Refusal> {
        if !participant.starts_with(tilo_actions::AGENT_TASK_PARTICIPANT_PREFIX)
            || name.trim().is_empty()
            || name.len() > MAX_AGENT_NAME_BYTES
        {
            return Err(Refusal::new(
                "only an agent task is named, with a short name",
            ));
        }
        self.agent_names.insert(participant, name);
        Ok(())
    }

    // The room's channels and categories are its layout, kept by those who
    // moderate it (as a server's channels are kept by those who may manage
    // them): a member talks in the channels, and does not make, rename,
    // move or delete them.

    /// New category.
    #[action(moderation)]
    pub fn create_category(
        &mut self,
        cx: &mut ActionCx,
        #[param("Category name")] name: String,
    ) -> Result<(), Refusal> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Refusal::new("a category needs a name"));
        }
        self.categories.push(
            cx.id(),
            Category {
                name: name.to_owned(),
            },
        );
        Ok(())
    }

    /// New channel.
    #[action(moderation)]
    pub fn create_channel(
        &mut self,
        cx: &mut ActionCx,
        #[param("Channel name")] name: String,
        #[param("The category to list it under; none for uncategorised")] category: Option<Id>,
        #[param("text for a conversation, voice for a call; text when absent")] kind: Option<
            ChannelKind,
        >,
        #[param("What the channel is about")] topic: Option<String>,
    ) -> Result<(), Refusal> {
        if let Some(category) = &category
            && !self.categories.contains(category)
        {
            return Err(Refusal::new("there is no such category"));
        }
        let name = name.trim().trim_start_matches('#');
        if name.is_empty() {
            return Err(Refusal::new("a channel needs a name"));
        }
        let id = cx.id();
        let kind = kind.unwrap_or_default();
        self.open_channel(
            id,
            Channel {
                category,
                name: name.to_owned(),
                topic: topic
                    .map(|topic| topic.trim().to_owned())
                    .unwrap_or_default(),
                kind,
                ..Channel::default()
            },
        );
        Ok(())
    }

    /// Delete category.
    ///
    /// Its channels stay, uncategorised. A category that no longer reads
    /// (held out of the chat, [`Collection::quarantined`]) is deleted too.
    #[action(item = categories, destructive, moderation)]
    pub fn delete_category(&mut self, category: &Id) -> Result<(), Refusal> {
        if self.categories.remove(category).is_none() && !self.categories.discard(category) {
            return Err(Refusal::new("there is no such category"));
        }
        for (_, channel) in self.channels.iter_mut() {
            if channel.category.as_ref() == Some(category) {
                channel.category = None;
            }
        }
        Ok(())
    }

    /// Move a channel.
    ///
    /// One place up or down among the channels of its category.
    #[action(item = channels, moderation)]
    pub fn move_channel(&mut self, channel: &Id, up: bool) -> Result<(), Refusal> {
        let moving = self
            .channels
            .get(channel)
            .ok_or_else(|| Refusal::new("there is no such channel"))?;
        let siblings: Vec<Id> = self
            .channels
            .iter()
            .filter(|(_, other)| other.category == moving.category)
            .map(|(id, _)| id.clone())
            .collect();
        let at = siblings
            .iter()
            .position(|id| id == channel)
            .unwrap_or_default();
        let neighbour = if up {
            at.checked_sub(1).and_then(|before| siblings.get(before))
        } else {
            siblings.get(at.saturating_add(1))
        };
        let Some(neighbour) = neighbour.cloned() else {
            return Ok(());
        };
        let target = self.channels.position(&neighbour).unwrap_or_default();
        self.channels.move_to(channel, target);
        Ok(())
    }

    /// File a channel.
    ///
    /// Lists it under another category, or under none.
    #[action(item = channels, moderation)]
    pub fn set_channel_category(
        &mut self,
        channel: &Id,
        #[param("The category to list it under; none for uncategorised")] category: Option<Id>,
    ) -> Result<(), Refusal> {
        if !self.channels.contains(channel) {
            return Err(Refusal::new("there is no such channel"));
        }
        if let Some(category) = &category
            && !self.categories.contains(category)
        {
            return Err(Refusal::new("there is no such category"));
        }
        if let Some(filed) = self.channels.get_mut(channel) {
            filed.category = category;
        }
        Ok(())
    }

    /// Rename channel.
    #[action(item = channels, moderation)]
    pub fn rename_channel(
        &mut self,
        channel: &Id,
        #[param("The channel's new name")] name: String,
    ) -> Result<(), Refusal> {
        let name = name.trim().trim_start_matches('#').trim();
        if name.is_empty() {
            return Err(Refusal::new("a channel needs a name"));
        }
        let renamed = self
            .channels
            .get_mut(channel)
            .ok_or_else(|| Refusal::new("there is no such channel"))?;
        name.clone_into(&mut renamed.name);
        Ok(())
    }

    /// Set channel topic.
    ///
    /// Empty for none.
    #[action(item = channels, moderation)]
    pub fn set_channel_topic(
        &mut self,
        channel: &Id,
        #[param("What the channel is about; empty for none")] topic: String,
    ) -> Result<(), Refusal> {
        topic.trim().clone_into(
            &mut self
                .channels
                .get_mut(channel)
                .ok_or_else(|| Refusal::new("there is no such channel"))?
                .topic,
        );
        Ok(())
    }

    /// Archive channel.
    ///
    /// Or bring an archived one back to the sidebar.
    #[action(item = channels, moderation)]
    pub fn archive_channel(
        &mut self,
        channel: &Id,
        #[param("Whether it leaves the sidebar")] archived: bool,
    ) -> Result<(), Refusal> {
        self.channels
            .get_mut(channel)
            .ok_or_else(|| Refusal::new("there is no such channel"))?
            .archived = archived;
        Ok(())
    }

    /// Rename category.
    #[action(item = categories, moderation)]
    pub fn rename_category(
        &mut self,
        category: &Id,
        #[param("The category's new name")] name: String,
    ) -> Result<(), Refusal> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Refusal::new("a category needs a name"));
        }
        let renamed = self
            .categories
            .get_mut(category)
            .ok_or_else(|| Refusal::new("there is no such category"))?;
        name.clone_into(&mut renamed.name);
        Ok(())
    }

    /// Delete channel.
    ///
    /// With its read marks. Its messages stay in the parts that
    /// hold them, shown nowhere. A channel that no longer reads (held out of
    /// the chat, [`Collection::quarantined`]) is deleted too.
    #[action(item = channels, destructive, moderation)]
    pub fn delete_channel(&mut self, channel: &Id) -> Result<(), Refusal> {
        if self.channels.remove(channel).is_none() && !self.channels.discard(channel) {
            return Err(Refusal::new("there is no such channel"));
        }
        let key = channel.to_string();
        for marks in self.read.values_mut() {
            marks.remove(&key);
        }
        Ok(())
    }

    /// Report message.
    ///
    /// Tells the room's moderators that this message, and its author,
    /// should not be here.
    #[action]
    pub fn report_message(
        &mut self,
        cx: &mut ActionCx,
        #[param("The message")] message: String,
        #[param("Who wrote it")] author: String,
        #[param("What is wrong with it")] reason: String,
    ) -> Result<(), Refusal> {
        if author.is_empty() || author.len() > 4096 {
            return Err(Refusal::new("a reported message is someone's"));
        }
        let person = author;
        self.moderation.report(
            cx,
            person,
            Some("so.tilo.chat".to_owned()),
            None,
            Some(message),
            reason,
        )
    }

    /// Report person.
    ///
    /// Tells the room's moderators that this person is misbehaving.
    #[action(item = people)]
    pub fn report_person(
        &mut self,
        cx: &mut ActionCx,
        person: &Id,
        #[param("What they did")] reason: String,
    ) -> Result<(), Refusal> {
        if self.people.find(person).is_none() {
            return Err(Refusal::new("there is no such person"));
        }
        self.moderation
            .report(cx, person.to_string(), None, None, None, reason)
    }

    /// Decide on a report.
    ///
    /// Records what was done about a person and why, settling their
    /// reports; the room's governance and history carry it out.
    #[action(moderation)]
    pub fn decide_reports(
        &mut self,
        cx: &mut ActionCx,
        #[param("A new id for the decision")] id: String,
        #[param("The person decided on")] person: String,
        #[param("The reports it settles")] reports: Vec<String>,
        #[param("What is done, and why")] ruling: moderation_model::Ruling,
    ) -> Result<(), Refusal> {
        self.moderation.decide(cx, id, person, reports, ruling)
    }

    /// Overturn a decision.
    ///
    /// A Moderator's word against an earlier decision; the host lifts what
    /// it imposed and restores what it took back.
    #[action(moderation)]
    pub fn overturn_decision(
        &mut self,
        cx: &mut ActionCx,
        #[param("The decision")] decision: String,
        #[param("Why")] note: String,
    ) -> Result<(), Refusal> {
        self.moderation.overturn(cx, decision, note)
    }

    /// Mark as read.
    ///
    /// The channel, for you, as far as you read it: every top-level message
    /// in the parts before `part`, and the first `seen` of them in `part`. A
    /// mark only moves forward.
    #[action(item = channels, housekeeping)]
    pub fn mark_read(
        &mut self,
        cx: &mut ActionCx,
        channel: &Id,
        #[param("The part of the chat's messages you read into")] part: u64,
        #[param("How many of the channel's top-level messages you saw in that part")] seen: u32,
    ) -> Result<(), Refusal> {
        let me = author(cx)?;
        if !self.channels.contains(channel) {
            return Err(Refusal::new("there is no such channel"));
        }
        let read = crate::parts::ReadMark {
            part: i64::try_from(part).unwrap_or(i64::MAX),
            seen: i64::from(seen),
        };
        let marks = self.read.entry(me).or_default();
        let mark = marks.entry(channel.to_string()).or_default();
        if *mark < read {
            *mark = read;
        }
        Ok(())
    }

    /// Submit feedback.
    ///
    /// A reviewed report, filed in the channel and kept with its triage;
    /// the channel shows it among its messages.
    // Filing a report is commenting: whoever may comment in the room may.
    #[action(item = channels, field = "comments")]
    pub fn submit_feedback(
        &mut self,
        cx: &mut ActionCx,
        channel: &Id,
        #[param("The reviewed submission")] submission: serde_json::Value,
    ) -> Result<(), Refusal> {
        let submission: ui::feedback::Submission = serde_json::from_value(submission)
            .map_err(|_| Refusal::new("a reviewed feedback submission is required"))?;
        if submission.destination.channel != channel.as_str() {
            return Err(Refusal::new("the submission is for another channel"));
        }
        submission
            .validate()
            .map_err(|error| Refusal::new(error.to_string()))?;
        let me = author(cx)?;
        let ts = seconds(cx);
        let key = Id::from_raw(submission.id.clone());
        if let Some(known) = self.feedback.get(&key) {
            return if known.submission.as_ref() == Some(&submission) && known.author.0 == me {
                Ok(())
            } else {
                Err(Refusal::new("feedback id conflict"))
            };
        }
        if !self.channels.contains(channel) {
            return Err(Refusal::new("there is no such channel"));
        }
        self.feedback.push(
            key,
            FeedbackReport {
                submission: Some(submission),
                author: Atom(me),
                ts,
                triage: Collection::new(),
            },
        );
        Ok(())
    }

    /// Triage feedback.
    ///
    /// Investigating, resolved, reopened, a duplicate of another report or
    /// tracked by a task.
    #[action(item = feedback)]
    pub fn triage(
        &mut self,
        cx: &mut ActionCx,
        report: &Id,
        #[param("investigating, resolved, reopen, duplicate or task")] update: serde_json::Value,
    ) -> Result<(), Refusal> {
        let update: crate::feedback::TriageUpdate = serde_json::from_value(update)
            .map_err(|_| Refusal::new("a triage update is required"))?;
        let me = author(cx)?;
        let ts = seconds(cx);
        let id = cx.id();
        self.feedback
            .get_mut(report)
            .ok_or_else(|| Refusal::new("there is no such report"))?
            .triage
            .push(
                id,
                Triage {
                    author: Atom(me),
                    ts,
                    update: Some(update),
                },
            );
        Ok(())
    }
}

#[cfg(test)]
mod migration_tests;
#[cfg(test)]
mod terminal_tests;
#[cfg(test)]
mod tests;
