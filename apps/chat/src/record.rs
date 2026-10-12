//! The chat record: workspaces, their categories and channels, every
//! channel's messages, the people in the room, each person's read marks and
//! the composers' drafts.
//!
//! Plain changes are the built-in field edits every path offers: renaming a
//! channel or a category, setting a channel's topic, archiving it, pinning a
//! message, typing in a draft. The methods of [`Chat`] are the operations
//! that are more than setting a field: posting, editing and deleting a
//! message, reacting, creating and reordering channels, an agent's record of
//! its work.
//!
//! Who posts, reacts or reads is whoever invokes the verb ([`ActionCx::author`]),
//! never an argument; when is the action's time ([`ActionCx::now_ms`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tilo_document::{ClipBlock, RichText};
use ui::tilo_resource::{
    self, ActionCx, Atom, Collection, Id, Identified, Json, JsonSchema, Keyed, Model, Refusal,
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

/// The room's chat. See the module docs.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Chat {
    /// The workspaces, in the order the rail shows them.
    pub workspaces: Collection<Workspace>,
    /// The channel categories of every workspace, in sidebar order.
    pub categories: Collection<Category>,
    /// The channels of every workspace, in sidebar order within their
    /// category.
    pub channels: Collection<Channel>,
    /// Every message of every channel, oldest first.
    pub messages: Collection<Message>,
    /// The people and agents in the room, by their participant id.
    pub people: Keyed<Person>,
    /// The name every member shows for an agent task, as the person hosting
    /// it labelled it.
    pub agent_names: BTreeMap<String, String>,
    /// Each person's read mark per channel: how many of its top-level
    /// messages they have seen.
    pub read: BTreeMap<String, BTreeMap<String, i64>>,
    /// The composers' drafts, by channel or thread.
    pub drafts: BTreeMap<String, RichText>,
    /// Reviewed feedback filed in a channel, under its submission's id.
    pub feedback: Collection<FeedbackReport>,
}

/// A workspace: the left rail's entry.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Workspace {
    /// The workspace's name.
    #[model(label)]
    pub name: String,
    /// The monogram the rail shows.
    pub glyph: String,
    /// Its colour in the rail.
    pub color: u32,
}

/// A collapsible group of channels within a workspace.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Category {
    /// The workspace it belongs to.
    #[model(readonly)]
    pub workspace: Id,
    /// The category's name.
    #[model(label)]
    pub name: String,
}

/// A text or voice channel.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Channel {
    /// The workspace it belongs to.
    #[model(readonly)]
    pub workspace: Id,
    /// The category it is listed under; none for uncategorised.
    pub category: Option<Id>,
    /// The channel's name.
    #[model(label)]
    pub name: String,
    /// What the channel is about.
    pub topic: String,
    /// A text channel has a conversation; a voice channel has a call.
    #[model(readonly)]
    pub kind: ChannelKind,
    /// Archived channels leave the sidebar and stay in the record.
    pub archived: bool,
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
    pub body: Json<Vec<ClipBlock>>,
    /// Whether it was changed after it was posted.
    #[model(readonly)]
    pub edited: bool,
    /// Its earlier texts, oldest first.
    #[model(readonly)]
    pub history: Json<Vec<Revision>>,
    /// The message it answers in a thread.
    #[model(readonly)]
    pub parent: Option<Id>,
    /// Who reacted with which emoji: one element per person and emoji, so
    /// two people's first reactions with one emoji both count.
    #[model(readonly)]
    pub reactions: ui::tilo_resource::Set<Reacted>,
    /// Who the text refers to, resolved when it was written.
    #[model(readonly)]
    pub mentions: Json<Vec<crate::mention::Mention>>,
    /// The thing it is about, when it was written by sharing something.
    #[model(readonly)]
    pub reference: Json<Option<tilo_reference::ResourceRef>>,
    /// The record the reference lives in, when the host resolved one.
    #[model(readonly)]
    pub source_provider: Option<Atom>,
    /// A file, an image or a video, by its content address.
    #[model(readonly)]
    pub attachment: Json<Option<crate::model::Attachment>>,
    /// Pinned messages are listed for everyone.
    pub pinned: bool,
    /// An agent's record of what it is doing with the message this one
    /// answers.
    #[model(readonly)]
    pub work: Option<Work>,
}

impl Default for Category {
    fn default() -> Self {
        Self {
            workspace: Id::from_raw(""),
            name: String::new(),
        }
    }
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            workspace: Id::from_raw(""),
            category: None,
            name: String::new(),
            topic: String::new(),
            kind: ChannelKind::default(),
            archived: false,
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
            body: Json::default(),
            edited: false,
            history: Json::default(),
            parent: None,
            reactions: ui::tilo_resource::Set::new(),
            mentions: Json::default(),
            reference: Json::default(),
            source_provider: None,
            attachment: Json::default(),
            pinned: false,
            work: None,
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Where it is, and since when: one authored claim, never mixed with
    /// another's time.
    pub state: Json<Option<Transition>>,
    /// How it decided, in the decider's words.
    pub reasoning: Json<Option<crate::model::AgentReasoning>>,
    /// One row per operation it dispatched, rewritten when the app answers.
    pub trace: Keyed<Step>,
}

/// One state of an agent's work and when it entered it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    pub outcome: Json<Option<crate::model::AgentStepOutcome>>,
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
    pub submission: Json<Option<ui::feedback::Submission>>,
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
    pub update: Json<Option<crate::feedback::TriageUpdate>>,
}

/// What an agent records of its work, as a verb takes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkUpdate {
    /// `taken`, `waiting`, `doing`, `done` or `refused`.
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

/// Who an action is by; refused without one, since a post, a reaction or a
/// read mark is always someone's.
fn author(cx: &ActionCx) -> Result<String, Refusal> {
    cx.author()
        .filter(|author| !author.is_empty() && author.len() <= 4096)
        .map(str::to_owned)
        .ok_or_else(|| Refusal::new("an action in chat is always someone's"))
}

fn seconds(cx: &ActionCx) -> i64 {
    cx.now_ms().div_euclid(1000)
}

fn valid_attachment(attachment: &crate::model::Attachment) -> bool {
    use crate::model::Attachment;
    let valid_blob =
        |blob: &str| blob.len() == 64 && blob.bytes().all(|byte| byte.is_ascii_hexdigit());
    let valid_name = |name: &str| {
        !name.is_empty()
            && name.len() <= MAX_ATTACHMENT_NAME_BYTES
            && !name.chars().any(char::is_control)
    };
    match attachment {
        Attachment::Image { name, blob, w, h } => {
            valid_name(name)
                && valid_blob(blob)
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
                && valid_blob(blob)
                && (1..=MAX_ATTACHMENT_BYTES).contains(size)
                && *w <= MAX_ATTACHMENT_DIMENSION
                && *h <= MAX_ATTACHMENT_DIMENSION
        }
        Attachment::File { name, blob, size } => {
            valid_name(name) && valid_blob(blob) && (1..=MAX_ATTACHMENT_BYTES).contains(size)
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

/// A monogram for a workspace glyph: the initials of its first two words,
/// as written.
pub(crate) fn monogram(name: &str) -> String {
    let letters: String = name
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .collect();
    if letters.is_empty() {
        "WS".to_owned()
    } else {
        letters
    }
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

/// A new message's fields beyond its channel, author and time.
#[derive(Default)]
pub(crate) struct Posting {
    pub text: String,
    pub body: Vec<ClipBlock>,
    pub parent: Option<Id>,
    pub attachment: Option<crate::model::Attachment>,
    pub reference: Option<tilo_reference::ResourceRef>,
    pub source_provider: Option<String>,
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
    fn mentions(&self, text: &str, body: &[ClipBlock]) -> Vec<crate::mention::Mention> {
        if body.is_empty() {
            return crate::mention::resolve(text, &self.participants());
        }
        let blocks: Vec<_> = body.iter().map(tilo_rich_text::content_of_clip).collect();
        crate::body::mentions(&blocks, &self.participants())
    }

    /// The top-level messages of `channel`.
    pub fn top_level(&self, channel: &Id) -> usize {
        self.messages
            .values()
            .filter(|message| message.channel == *channel && message.parent.is_none())
            .count()
    }

    /// Append a message to `channel` by `author` at `ts`, refusing what no
    /// one should see: a voice channel's timeline, a reply to a reply, an
    /// oversized body or an invalid attachment.
    pub(crate) fn append(
        &mut self,
        id: Id,
        channel: &Id,
        author: String,
        ts: i64,
        posting: Posting,
    ) -> Result<Id, Refusal> {
        let target = self
            .channels
            .get(channel)
            .ok_or_else(|| Refusal::new("there is no such channel"))?;
        if target.kind == ChannelKind::Voice {
            return Err(Refusal::new(
                "a voice channel has no conversation to post in",
            ));
        }
        if posting.text.len() > MAX_MESSAGE_TEXT_BYTES {
            return Err(Refusal::new("the message is too long"));
        }
        if posting
            .attachment
            .as_ref()
            .is_some_and(|attachment| !valid_attachment(attachment))
        {
            return Err(Refusal::new(
                "the attachment is not a valid content reference",
            ));
        }
        if let Some(parent) = &posting.parent
            && let Some(parent) = self.messages.get(parent)
            && (parent.parent.is_some() || parent.channel != *channel)
        {
            return Err(Refusal::new(
                "a reply answers a top-level message of its channel",
            ));
        }
        if posting
            .source_provider
            .as_deref()
            .is_some_and(|provider| !valid_source_provider(provider))
        {
            return Err(Refusal::new("an existing record provider is required"));
        }
        let mentions = self.mentions(&posting.text, &posting.body);
        self.messages.push(
            id.clone(),
            Message {
                channel: channel.clone(),
                author: Atom(author),
                ts,
                text: posting.text,
                body: Json(posting.body),
                parent: posting.parent,
                mentions: Json(mentions),
                reference: Json(posting.reference),
                source_provider: posting.source_provider.map(Atom),
                attachment: Json(posting.attachment),
                ..Message::default()
            },
        );
        Ok(id)
    }

    /// Add `channel` as `id`, with the empty draft its composer writes in:
    /// made with the channel, so collaborators' first keystrokes land in
    /// one draft rather than in two made at once.
    pub(crate) fn open_channel(&mut self, id: Id, channel: Channel) {
        if channel.kind == ChannelKind::Text {
            self.drafts.insert(id.to_string(), RichText::default());
        }
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

    /// Start a fresh record: its creator, a workspace and, without one named
    /// by the host, two channels to start in.
    pub(crate) fn begin(
        &mut self,
        creator: &str,
        workspace: Option<(&str, &str)>,
        (name, initials, color): (&str, &str, u32),
    ) -> Result<(), Refusal> {
        if let Some((id, _)) = workspace
            && !self.workspaces.is_empty()
        {
            return if self.workspaces.contains(&Id::from_raw(id)) {
                Ok(())
            } else {
                Err(Refusal::new("chat is already set up for another workspace"))
            };
        }
        if !self.workspaces.is_empty() {
            return Ok(());
        }
        let person = self.person(creator)?;
        name.clone_into(&mut person.name);
        initials.clone_into(&mut person.initials);
        person.color = color;
        person.status = Presence::Online;
        match workspace {
            Some((id, label)) => {
                let label = if label.trim().is_empty() { id } else { label };
                self.workspaces.push(
                    Id::from_raw(id),
                    Workspace {
                        name: label.to_owned(),
                        glyph: monogram(label),
                        color: 0,
                    },
                );
            }
            None => self.starter(),
        }
        Ok(())
    }

    /// The first workspace of a record no host named one for: `#general` to
    /// talk in and `Lounge` to call in.
    fn starter(&mut self) {
        let home = Id::from_raw("ws_home");
        self.workspaces.push(
            home.clone(),
            Workspace {
                name: "My Workspace".into(),
                glyph: "MW".into(),
                color: 0,
            },
        );
        for (id, name) in [("cat_home_text", "Channels"), ("cat_home_voice", "Voice")] {
            self.categories.push(
                Id::from_raw(id),
                Category {
                    workspace: home.clone(),
                    name: name.into(),
                },
            );
        }
        for (id, category, name, kind) in [
            ("c_general", "cat_home_text", "general", ChannelKind::Text),
            ("c_lounge", "cat_home_voice", "Lounge", ChannelKind::Voice),
        ] {
            self.open_channel(
                Id::from_raw(id),
                Channel {
                    workspace: home.clone(),
                    category: Some(Id::from_raw(category)),
                    name: name.into(),
                    kind,
                    ..Channel::default()
                },
            );
        }
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
)]
impl Chat {
    /// Start the chat.
    ///
    /// Its creator and their first workspace: the one the host names, or a
    /// starter workspace with `#general` and a voice lounge.
    #[action(housekeeping)]
    pub fn begin_chat(
        &mut self,
        cx: &mut ActionCx,
        #[param("The workspace the host admitted; none for a starter workspace")] workspace: Option<
            String,
        >,
        #[param("The workspace's name")] workspace_name: Option<String>,
        #[param("Your name")] name: String,
        #[param("Your initials")] initials: String,
        #[param("Your colour")] color: u32,
    ) -> Result<(), Refusal> {
        let creator = author(cx)?;
        if name.len() > 128 || initials.len() > 8 {
            return Err(Refusal::new("the creator's name is too long"));
        }
        let workspace_name = workspace_name.unwrap_or_default();
        self.begin(
            &creator,
            workspace.as_deref().map(|id| (id, workspace_name.as_str())),
            (&name, &initials, color),
        )
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

    /// Create a workspace.
    ///
    /// With `#general` to talk in and `Lounge` to call in.
    #[action]
    pub fn create_workspace(
        &mut self,
        cx: &mut ActionCx,
        #[param("Workspace name")] name: String,
    ) -> Result<(), Refusal> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Refusal::new("a workspace needs a name"));
        }
        let workspace = cx.id();
        self.workspaces.push(
            workspace.clone(),
            Workspace {
                name: name.to_owned(),
                glyph: monogram(name),
                color: u32::try_from(self.workspaces.len() % 8).unwrap_or_default(),
            },
        );
        for (category, channel, kind) in [
            ("Channels", "general", ChannelKind::Text),
            ("Voice", "Lounge", ChannelKind::Voice),
        ] {
            let category_id = cx.id();
            self.categories.push(
                category_id.clone(),
                Category {
                    workspace: workspace.clone(),
                    name: category.into(),
                },
            );
            self.open_channel(
                cx.id(),
                Channel {
                    workspace: workspace.clone(),
                    category: Some(category_id),
                    name: channel.into(),
                    kind,
                    ..Channel::default()
                },
            );
        }
        Ok(())
    }

    /// New category.
    #[action(item = workspaces)]
    pub fn create_category(
        &mut self,
        cx: &mut ActionCx,
        workspace: &Id,
        #[param("Category name")] name: String,
    ) -> Result<(), Refusal> {
        if !self.workspaces.contains(workspace) {
            return Err(Refusal::new("there is no such workspace"));
        }
        let name = name.trim();
        if name.is_empty() {
            return Err(Refusal::new("a category needs a name"));
        }
        self.categories.push(
            cx.id(),
            Category {
                workspace: workspace.clone(),
                name: name.to_owned(),
            },
        );
        Ok(())
    }

    /// New channel.
    #[action(item = workspaces)]
    pub fn create_channel(
        &mut self,
        cx: &mut ActionCx,
        workspace: &Id,
        #[param("Channel name")] name: String,
        #[param("The category to list it under; none for uncategorised")] category: Option<Id>,
        #[param("text for a conversation, voice for a call; text when absent")] kind: Option<
            ChannelKind,
        >,
    ) -> Result<(), Refusal> {
        if !self.workspaces.contains(workspace) {
            return Err(Refusal::new("there is no such workspace"));
        }
        if category
            .as_ref()
            .is_some_and(|category| !self.categories.contains(category))
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
                workspace: workspace.clone(),
                category,
                name: name.to_owned(),
                kind,
                ..Channel::default()
            },
        );
        Ok(())
    }

    /// Delete category.
    ///
    /// Its channels stay, uncategorised.
    #[action(item = categories, destructive)]
    pub fn delete_category(&mut self, category: &Id) -> Result<(), Refusal> {
        self.categories
            .remove(category)
            .ok_or_else(|| Refusal::new("there is no such category"))?;
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
    #[action(item = channels)]
    pub fn move_channel(&mut self, channel: &Id, up: bool) -> Result<(), Refusal> {
        let moving = self
            .channels
            .get(channel)
            .ok_or_else(|| Refusal::new("there is no such channel"))?;
        let siblings: Vec<Id> = self
            .channels
            .iter()
            .filter(|(_, other)| {
                other.workspace == moving.workspace && other.category == moving.category
            })
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

    /// Delete channel.
    ///
    /// With its messages and drafts.
    #[action(item = channels, destructive)]
    pub fn delete_channel(&mut self, channel: &Id) -> Result<(), Refusal> {
        self.channels
            .remove(channel)
            .ok_or_else(|| Refusal::new("there is no such channel"))?;
        self.messages
            .retain(|_, message| message.channel != *channel);
        let key = channel.to_string();
        let thread = format!("{key}:");
        self.drafts
            .retain(|draft, _| *draft != key && !draft.starts_with(&thread));
        for marks in self.read.values_mut() {
            marks.remove(&key);
        }
        Ok(())
    }

    /// Start a draft.
    ///
    /// The empty draft a composer writes in, made once: a thread's the
    /// first time someone answers in it.
    #[action(housekeeping)]
    pub fn start_draft(
        &mut self,
        #[param("The channel, or the channel and thread, it is written in")] key: String,
    ) -> Result<(), Refusal> {
        if key.is_empty() || key.len() > 512 {
            return Err(Refusal::new("a draft is kept under its channel or thread"));
        }
        self.drafts.entry(key).or_default();
        Ok(())
    }

    /// Post a message.
    ///
    /// `@Name` of a member becomes a mention of them.
    #[action(item = channels)]
    pub fn post(
        &mut self,
        cx: &mut ActionCx,
        channel: &Id,
        #[param("The message; @Name mentions a member")] text: String,
        #[param("The message it answers in a thread")] parent: Option<Id>,
        #[param("Its paragraphs as written, with their formatting")] body: Option<Vec<ClipBlock>>,
        #[param("A file, image or video another action put in the store")] attachment: Option<
            AttachmentUpdate,
        >,
    ) -> Result<(), Refusal> {
        let attachment = match attachment {
            Some(update) => Some(
                update
                    .attachment()
                    .ok_or_else(|| Refusal::new("an attachment is an image, a video or a file"))?,
            ),
            None => None,
        };
        if text.trim().is_empty() && attachment.is_none() && parent.is_none() {
            return Err(Refusal::new("a message says something"));
        }
        let id = cx.id();
        let by = author(cx)?;
        let ts = seconds(cx);
        self.append(
            id,
            channel,
            by,
            ts,
            Posting {
                text,
                body: body.unwrap_or_default(),
                parent,
                attachment,
                ..Posting::default()
            },
        )
        .map(drop)
    }

    /// Share live resource.
    ///
    /// Post the address of something, with words about it; whoever opens it
    /// still needs their own access to it.
    #[action(item = channels)]
    pub fn share_resource(
        &mut self,
        cx: &mut ActionCx,
        channel: &Id,
        #[param("The thing: its kind, id, label and part")] reference: SharedReference,
        #[param("Words accompanying it")] text: Option<String>,
        #[param("The record it lives in; needed to open or copy a link to it")]
        source_provider: Option<String>,
    ) -> Result<(), Refusal> {
        let span = match reference.span {
            Some(span) => Some(tilo_reference::Span {
                start: span.start,
                end: span.end,
                unit: match span.unit.as_str() {
                    "Bytes" => tilo_reference::SpanUnit::Bytes,
                    _ => tilo_reference::SpanUnit::Chars,
                },
            }),
            None => None,
        };
        let reference = tilo_reference::ResourceRef {
            kind: reference.kind,
            id: reference.id,
            label: reference.label,
            span,
        };
        if !valid_reference(&reference) {
            return Err(Refusal::new("a bounded Tilo resource address is required"));
        }
        let text = text
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| reference.label.clone());
        let id = cx.id();
        let by = author(cx)?;
        let ts = seconds(cx);
        self.append(
            id,
            channel,
            by,
            ts,
            Posting {
                text,
                reference: Some(reference),
                source_provider,
                ..Posting::default()
            },
        )
        .map(drop)
    }

    /// Edit message.
    ///
    /// Its earlier text is kept in its history.
    #[action(item = messages)]
    pub fn edit_message(
        &mut self,
        cx: &mut ActionCx,
        message: &Id,
        text: String,
        body: Option<Vec<ClipBlock>>,
    ) -> Result<(), Refusal> {
        if text.len() > MAX_MESSAGE_TEXT_BYTES {
            return Err(Refusal::new("the message is too long"));
        }
        let ts = seconds(cx);
        let body = body.unwrap_or_default();
        let mentions = self.mentions(&text, &body);
        let row = self
            .messages
            .get_mut(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        if row.text == text && row.body.0 == body {
            return Ok(());
        }
        if row.text != text {
            row.history.0.push(Revision {
                ts,
                text: std::mem::take(&mut row.text),
            });
            row.mentions = Json(mentions);
            row.text = text;
        }
        row.body = Json(body);
        row.edited = true;
        Ok(())
    }

    /// Delete message.
    #[action(item = messages, destructive)]
    pub fn delete_message(&mut self, message: &Id) -> Result<(), Refusal> {
        self.messages
            .remove(message)
            .map(drop)
            .ok_or_else(|| Refusal::new("there is no such message"))
    }

    /// React.
    ///
    /// Your reaction with an emoji.
    #[action(item = messages)]
    pub fn react(&mut self, cx: &mut ActionCx, message: &Id, emoji: String) -> Result<(), Refusal> {
        let me = author(cx)?;
        let emoji = emoji.trim().to_owned();
        if emoji.is_empty() || emoji.len() > 64 {
            return Err(Refusal::new("a reaction is one emoji"));
        }
        let row = self
            .messages
            .get_mut(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        row.reactions.insert(Reacted { emoji, user: me });
        Ok(())
    }

    /// Take back a reaction.
    #[action(item = messages)]
    pub fn unreact(
        &mut self,
        cx: &mut ActionCx,
        message: &Id,
        emoji: String,
    ) -> Result<(), Refusal> {
        let me = author(cx)?;
        let row = self
            .messages
            .get_mut(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        row.reactions.remove(&Reacted {
            emoji: emoji.trim().to_owned(),
            user: me,
        });
        Ok(())
    }

    /// Mark as read.
    ///
    /// Everything in the channel so far, for you.
    #[action(item = channels, housekeeping)]
    pub fn mark_read(
        &mut self,
        cx: &mut ActionCx,
        channel: &Id,
        #[param("How many of its top-level messages you saw; all when absent")] seen: Option<u32>,
    ) -> Result<(), Refusal> {
        let me = author(cx)?;
        if !self.channels.contains(channel) {
            return Err(Refusal::new("there is no such channel"));
        }
        let all = i64::try_from(self.top_level(channel)).unwrap_or(i64::MAX);
        let count = seen.map_or(all, |seen| all.min(i64::from(seen)));
        let marks = self.read.entry(me).or_default();
        let mark = marks.entry(channel.to_string()).or_default();
        if *mark < count {
            *mark = count;
        }
        Ok(())
    }

    /// Record agent work.
    ///
    /// An agent's account of what it is doing with the message that
    /// summoned it, kept on its reply to that message: the reply is made
    /// the first time, and `say` gives it its words once, while it has none.
    #[action(item = messages)]
    pub fn record_work(
        &mut self,
        cx: &mut ActionCx,
        message: &Id,
        #[param("Where the work is, what was done and why")] work: WorkUpdate,
        #[param("The reply's words, when it has none yet")] say: Option<String>,
    ) -> Result<(), Refusal> {
        let me = author(cx)?;
        let since = seconds(cx);
        let (state, reasoning, trace) = crate::model::AgentWork::from_update(&work)
            .map_err(Refusal::new)?
            .into_parts();
        let summons = self
            .messages
            .get(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        let channel = summons.channel.clone();
        let thread = summons.parent.clone().unwrap_or_else(|| message.clone());
        let reply = reply_id(message, &me);
        if !self.messages.contains(&reply) {
            self.append(
                reply.clone(),
                &channel,
                me.clone(),
                since,
                Posting {
                    parent: Some(thread),
                    ..Posting::default()
                },
            )?;
        }
        let row = self
            .messages
            .get_mut(&reply)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        if row.author.0 != me {
            return Err(Refusal::new("only the reply's author records its work"));
        }
        let record = row.work.get_or_insert_with(Work::default);
        let unchanged = record
            .state
            .0
            .as_ref()
            .is_some_and(|transition| transition.state == state);
        if !unchanged {
            record.state = Json(Some(Transition { state, since }));
        }
        // Each update says why, or that no reason is on record now.
        record.reasoning = Json(reasoning);
        for step in trace {
            let id = Id::from_raw(step.id.clone());
            let row = Step {
                id: Atom(step.id),
                sequence: step.sequence,
                resource: Atom(step.resource),
                label: step.label,
                outcome: Json(Some(step.outcome)),
            };
            if let Some(known) = record.trace.find_mut(&id) {
                *known = row;
            } else {
                record.trace.push(row);
            }
        }
        if let Some(say) = say
            && row.text.is_empty()
            && say.len() <= MAX_MESSAGE_TEXT_BYTES
        {
            row.text = say;
        }
        Ok(())
    }

    /// Submit feedback.
    ///
    /// A reviewed report, posted in the channel and kept with its triage.
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
            return if known.submission.0.as_ref() == Some(&submission) && known.author.0 == me {
                Ok(())
            } else {
                Err(Refusal::new("feedback id conflict"))
            };
        }
        let message = Id::from_raw(crate::feedback::message_id(&submission.id));
        self.append(
            message,
            channel,
            me.clone(),
            ts,
            Posting {
                text: submission.feedback.text.clone(),
                ..Posting::default()
            },
        )?;
        self.feedback.push(
            key,
            FeedbackReport {
                submission: Json(Some(submission)),
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
                    update: Json(Some(update)),
                },
            );
        Ok(())
    }
}

#[cfg(test)]
mod tests;
