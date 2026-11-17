//! The chat record: the workspace's categories and channels, the people in the
//! workspace, each person's read marks, reviewed feedback and moderation. Chat's
//! record is the chat of one Tilo workspace and holds no workspaces of its
//! own: every channel of the record is the workspace's. The channels'
//! messages are kept in parts beside it
//! ([`crate::parts`], [`crate::messages`]), so the record stays small
//! however long the chat runs. What someone is writing and has not sent
//! is theirs, not the workspace's: it is kept in their own record
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
use ui::tilo_resource::{
    self, ActionCx, Atom, Collection, Id, Identified, JsonSchema, Keyed, Model, NonBlank, Refusal,
};

use tilo_actions::policy::MemberId;

use crate::model::{ChannelKind, Presence};

/// Longest message body, in bytes: a conversation turn, not a document.
pub const MAX_MESSAGE_TEXT_BYTES: usize = 64 * 1024;
/// Longest agent display name recorded with provider output.
pub const MAX_AGENT_NAME_BYTES: usize = 80;
/// Largest attachment a message refers to.
pub const MAX_ATTACHMENT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ATTACHMENT_NAME_BYTES: usize = 255;
const MAX_ATTACHMENT_DIMENSION: u32 = 16_384;

/// The longest channel id a caller may choose.
const MAX_GIVEN_ID_CHARS: usize = 64;

/// A channel id a caller chose (`create-channel`'s `id`): letters, digits,
/// `-` and `_`, so it reads the same in an item address and a step.
fn given_channel_id(given: &str) -> Result<Id, Refusal> {
    let given = given.trim();
    if given.is_empty()
        || given.chars().count() > MAX_GIVEN_ID_CHARS
        || !given
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err(Refusal::new(format!(
            "`{given}` is not a channel id: letters, digits, - and _, at most \
             {MAX_GIVEN_ID_CHARS}"
        )));
    }
    Ok(Id::from_raw(given))
}

/// A channel's name as it is shown, without the `#` a person may type
/// before it: refused when that is all there is.
fn channel_name(name: &NonBlank) -> Result<&str, Refusal> {
    let name = name.trim_start_matches('#').trim_start();
    if name.is_empty() {
        return Err(Refusal::new("a channel needs a name"));
    }
    Ok(name)
}

/// One channel's place in [`Chat::arrange_channels`]: listed under
/// `category`, none for uncategorised.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChannelPlace {
    pub channel: String,
    #[serde(default)]
    pub category: Option<String>,
}

/// Put the items `order` names in that order. An item not named stays
/// with the named one before it (those before every named one stay
/// first), so what one side of a bridge lists alone keeps its neighbour.
fn arrange<T>(items: &mut Collection<T>, order: &[Id]) {
    let mut leading: Vec<Id> = Vec::new();
    let mut following: BTreeMap<&Id, Vec<Id>> = BTreeMap::new();
    let mut last: Option<&Id> = None;
    for id in items.ids() {
        match order.iter().find(|named| *named == id) {
            Some(named) => last = Some(named),
            None => match last {
                Some(named) => following.entry(named).or_default().push(id.clone()),
                None => leading.push(id.clone()),
            },
        }
    }
    let mut wanted = leading;
    for named in order {
        wanted.push(named.clone());
        wanted.extend(following.remove(named).unwrap_or_default());
    }
    for (index, id) in wanted.iter().enumerate() {
        if items.position(id) != Some(index) {
            items.move_to(id, index);
        }
    }
}

/// The id of the workspace's text channel every chat opens with, `#chat`.
pub const CHAT_CHANNEL: &str = "c_chat";
/// The id of the workspace's voice channel every chat opens with, `#voice`.
pub const VOICE_CHANNEL: &str = "c_voice";

/// Give a workspace opened for the first time, with no channels at all, its
/// first two: `#chat` to talk in and `#voice` to call in, under fixed ids
/// and with the same values, so every device that opens the workspace first
/// makes the very same two, and their copies are one item each. A workspace
/// with any channel, even one that does not read, gets nothing.
fn open_workspace(channels: &mut Collection<Conversation>) {
    if !channels.is_empty() || !channels.quarantined().is_empty() {
        return;
    }
    for (id, name, kind) in [
        (CHAT_CHANNEL, "chat", ChannelKind::Text),
        (VOICE_CHANNEL, "voice", ChannelKind::Voice),
    ] {
        channels.push(
            Id::from_raw(id),
            Conversation {
                name: name.into(),
                kind,
                ..Conversation::default()
            },
        );
    }
}

/// The workspace's chat. See the module docs.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Chat {
    /// The workspace's channel categories, in sidebar order: made, ordered and
    /// removed only by the record's actions, which keep them to those who
    /// keep the workspace's layout. A generic insert, move or removal would let
    /// anyone who may edit the workspace (a public workspace's visitor) rearrange it.
    #[model(readonly)]
    pub categories: Collection<Category>,
    /// The workspace's conversations, in sidebar order within their category:
    /// made, ordered and removed only by the record's actions, for the same
    /// reason as [`Self::categories`]. The field keeps the name records have
    /// always kept them by: a channel is a conversation the whole workspace
    /// has, named and listed; a direct conversation is one a set of
    /// members has, named by them and never stored
    /// ([`Chat::direct_id`]).
    #[model(readonly)]
    pub channels: Collection<Conversation>,
    /// The people and agents in the workspace, under [`member_slot`].
    pub people: Keyed<Person>,
    /// Each person's read mark per channel, under [`member_slot`]: how far
    /// into the chain of message parts they have read it.
    pub read: BTreeMap<String, BTreeMap<String, crate::parts::ReadMark>>,
    /// Reviewed feedback filed in a channel, under its submission's id.
    pub feedback: Collection<FeedbackReport>,
    /// Reports of people and messages, and what the workspace's moderators (or
    /// its host's moderation agent) decided on them.
    pub moderation: moderation_model::Moderation,
    /// What the workspace's host keeps of the files members post, as the host
    /// says: a member's Chat refuses, in plain words, a file the host would
    /// not take, rather than posting one that never reaches it.
    #[model(readonly)]
    pub files: WorkspaceFiles,
    /// What each agent keeps of a conversation it answers in, by the conversation's id and then
    /// under the agent's [`member_slot`]: the summary its turns are told in place of the
    /// messages they are no longer told as written ([`crate::recall`]). Kept here and not with
    /// the messages, so a device that holds only the newest part still has it. Written only by
    /// [`Chat::keep_summary`].
    #[model(readonly)]
    pub summaries: BTreeMap<String, BTreeMap<String, Summary>>,
}

/// What an agent keeps of a conversation it answers in: everything up to one message, in its own
/// words.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// The summary.
    pub text: String,
    /// The last message it takes in, by its id.
    pub through: Atom,
    /// When that message was written, in seconds since the epoch.
    pub through_ts: i64,
}

/// What a workspace's host keeps of the files its members post; zero where it
/// sets no bound (a chat no host keeps files for).
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceFiles {
    /// The largest file it keeps, in bytes.
    pub largest: u64,
    /// What it takes in of one member's files in any hour, in bytes.
    pub member_hourly: u64,
}

/// A collapsible group of the workspace's channels.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Category {
    /// The category's name: changed by renaming it
    /// ([`Chat::rename_category`]), which only those who keep the workspace's
    /// layout may.
    #[model(label, readonly)]
    pub name: String,
}

/// One conversation of the workspace: a channel every member has, or a
/// direct conversation between some of them.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Conversation {
    /// The category it is listed under; none for uncategorised: changed by
    /// [`Chat::set_channel_category`], which only those who keep the workspace's
    /// layout may.
    #[model(readonly)]
    pub category: Option<Id>,
    /// The channel's name: changed by renaming it
    /// ([`Chat::rename_channel`]), which only those who keep the workspace's
    /// layout may.
    #[model(label, readonly)]
    pub name: String,
    /// What the channel is about: changed by [`Chat::set_channel_topic`],
    /// which only those who keep the workspace's layout may.
    #[model(readonly)]
    pub topic: String,
    /// A text channel has a conversation; a voice channel has a call.
    #[model(readonly)]
    pub kind: ChannelKind,
    /// Archived channels leave the sidebar and stay in the record: changed
    /// by archiving it ([`Chat::archive_channel`]), which only those who
    /// keep the workspace's layout may.
    #[model(readonly)]
    pub archived: bool,
    /// Who the conversation is with: empty for a
    /// channel of the whole workspace. Not access control — every member
    /// reads every conversation; it decides where the conversation is
    /// listed and whose unread count moves.
    #[model(readonly)]
    pub members: Vec<MemberId>,
}

impl Conversation {
    /// Whether this is one of the workspace's channels: a conversation of
    /// all its members, which has no members of its own.
    #[must_use]
    pub fn is_channel(&self) -> bool {
        self.members.is_empty()
    }
}

/// The prefix of a direct conversation's id: the members it is with
/// follow it, in order, as JSON.
pub const DIRECT_CONVERSATION_PREFIX: &str = "direct:";

/// One message of a channel.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Message {
    /// The channel it was posted in.
    #[model(readonly)]
    pub channel: Id,
    #[model(readonly)]
    pub author: MemberId,
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
    /// The run of the answered message it quotes, when it answers a span rather than the whole
    /// message: what the quote over it shows, and where pressing the quote reveals it.
    #[model(readonly)]
    pub quoted: Option<Quote>,
    /// Who reacted with which emoji: the framework's reactions, each the
    /// invoker's own, so this read-only message still takes them.
    #[model(readonly)]
    pub reactions: ui::tilo_resource::Reactions,
    /// Who the text refers to, resolved when it was written.
    #[model(readonly)]
    pub mentions: Vec<crate::mention::Mention>,
    /// What it carries besides its words: the records and parts of records
    /// it was written by sharing, the words it quotes, and the files it
    /// posts, each in the terms of the app that keeps it.
    #[model(readonly)]
    pub attachments: Vec<tilo_reference::Attached>,
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
    /// The agent or app that wrote it for a person, as its send pinned them: what names it
    /// "Helper for Ada" after it left too. `None` for a person's own words.
    #[model(readonly)]
    pub acting: Option<ui::tilo_resource::Acting>,
    /// The name the author went by where it wrote, for an author that is not a person: what
    /// the host that admitted it called it, kept so every reader names it after it left.
    #[model(readonly)]
    pub author_name: Option<String>,
    /// Where it was first written, when it was moved here from another chat:
    /// shown as its author's, at its time there, while `author` keeps who
    /// moved it.
    #[model(readonly)]
    pub origin: Option<ui::tilo_resource::Origin>,
    /// The window it shares live, when it was written by sharing one: the
    /// resource is the one its first resource attachment names.
    #[model(readonly)]
    pub share: Option<WindowShare>,
    /// What it asks one person, and where that question stands: it waits
    /// until they answer it, on every device, or its asker takes it back
    /// ([`crate::question`]).
    #[model(readonly)]
    pub question: Option<crate::question::Question>,
    /// How many times a person asked the agent again for an answer to this
    /// message (`retry`): the attempt the agent answers in now. The answer of
    /// attempt `n` is the reply [`reply_id_at`] derives, so asking again never
    /// overwrites the reply that failed — the tries stand beside one another.
    #[model(readonly)]
    pub attempts: u32,
}

impl Message {
    /// `text` by `author` in `channel` at `ts`, with nothing else to it.
    #[must_use]
    pub fn new(channel: Id, author: MemberId, ts: i64, text: String) -> Self {
        Self {
            channel,
            author,
            ts,
            text,
            body: Vec::new(),
            edited: false,
            history: Vec::new(),
            parent: None,
            quoted: None,
            reactions: ui::tilo_resource::Reactions::new(),
            mentions: Vec::new(),
            attachments: Vec::new(),
            pinned: false,
            work: None,
            appearance: None,
            acting: None,
            author_name: None,
            origin: None,
            share: None,
            question: None,
            attempts: 0,
        }
    }
}

/// A window shared live into a channel: everyone who reads the channel sees
/// the resource as it is now, drawn by its own app from their own copy of
/// its record, and opens it with the access the sharer gave.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowShare {
    /// What the people who open it may do there.
    pub access: ShareAccess,
    /// Whether the sharer still has the window open: the embed says it is
    /// live while they do.
    pub open: bool,
    /// The sharer stopped sharing it: the message keeps saying what was
    /// shared, and no longer shows or opens it.
    pub ended: bool,
}

/// What the people a window is shared with may do when they open it.
#[derive(
    Model,
    Default,
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    ui::tilo_resource::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ShareAccess {
    /// Look at it: the window opens read-only.
    #[default]
    View,
    /// Work on it with the sharer.
    Edit,
}

impl ShareAccess {
    /// Its name in an action's argument and in the record.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Edit => "edit",
        }
    }
}

/// The name and avatar a bot showed for a message instead of its own, as the
/// message keeps it: [`tilo_actions::Appearance`] as stored values.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShownAs {
    /// The shown name.
    #[model(atom)]
    pub name: String,
    /// BLAKE3 hex of an image in the workspace's content store.
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
    /// The workspace member the account is linked to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked: Option<String>,
}

impl From<tilo_actions::Appearance> for ShownAs {
    fn from(appearance: tilo_actions::Appearance) -> Self {
        Self {
            name: appearance.name,
            avatar: appearance.avatar,
            persona: appearance.persona.map(|persona| ShownPersona {
                service: persona.service,
                account: persona.account,
                linked: persona.linked,
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
                linked: persona.linked,
            }),
        }
    }
}

/// An earlier text of a message.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    pub ts: i64,
    pub text: String,
}

/// A person or agent in the workspace.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Person {
    #[model(readonly)]
    pub id: MemberId,
    /// The name they show.
    #[model(label)]
    pub name: String,
    /// Their monogram.
    pub initials: String,
    /// Their colour.
    pub color: u32,
    /// Whether they are around.
    pub status: Presence,
}

impl Person {
    /// `id`, not named yet.
    #[must_use]
    pub fn unnamed(id: MemberId) -> Self {
        Self {
            id,
            name: String::new(),
            initials: String::new(),
            color: 0,
            status: Presence::default(),
        }
    }

    /// Whether this is software acting in the workspace rather than a person.
    #[must_use]
    pub fn is_agent(&self) -> bool {
        !self.id.is_person()
    }
}

impl Identified for Person {
    fn id(&self) -> Id {
        member_slot(&self.id)
    }
}

/// The key the record keeps `member`'s person and read marks under.
#[must_use]
pub fn member_slot(member: &MemberId) -> Id {
    Id::from_raw(ui::tilo_resource::text::author_mark(member))
}

/// The run of a message a reply quotes, as the reply carries it: the words as they read when
/// the reply was written, and where in the answered message they stand so the quote opens the
/// very words it names ([`crate::effects::ChatEffect::RevealMessageRange`]).
///
/// A reply that answers a whole message carries none: its `parent` is the message it answers,
/// and the quote over it shows the start of what that message says. A reply to a *span* of a
/// message carries one, and the quote shows the span instead.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Quote {
    /// The words as they read when the reply was written: what the quote shows, however the
    /// answered message changes after.
    #[model(label)]
    pub text: String,
    /// Where they stand in the answered message's text, in UTF-8 bytes of
    /// [`Message::text`]: what pressing the quote reveals. A body edited after the quote was
    /// taken may move the words; the span is kept as written, and the quote itself always
    /// reads from `text`.
    pub start: u64,
    /// Where the run ends, in UTF-8 bytes, exclusive.
    pub end: u64,
}

impl Quote {
    /// Whether the span a reply quotes is one this record may keep: its words are some and
    /// bounded, and its span is ordered and inside a message of at most
    /// [`MAX_MESSAGE_TEXT_BYTES`].
    #[must_use]
    pub fn valid(&self) -> bool {
        !self.text.trim().is_empty()
            && self.text.len() <= MAX_MESSAGE_TEXT_BYTES
            && self.start < self.end
            && usize::try_from(self.end).is_ok_and(|end| end <= MAX_MESSAGE_TEXT_BYTES)
    }
}

/// An agent's record of its work on a message, written by that agent.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Work {
    /// Incremental provider output.
    pub stream: Option<ProviderStream>,
    /// Where it is, and since when: one authored claim, never mixed with
    /// another's time.
    pub state: Option<Transition>,
    /// One row per operation it dispatched, rewritten when the app answers.
    pub trace: Keyed<Step>,
    /// The run's working record of a failure, for whoever debugs it later: the
    /// transport's error, the partial answer, the provider's exposed reasoning.
    /// Never the reply's words, and shown only behind a disclosure.
    pub failure_detail: Option<String>,
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
#[derive(Model, Debug, Clone, PartialEq)]
pub struct FeedbackReport {
    /// What was submitted, as the person reviewed it.
    #[model(readonly)]
    pub submission: Option<ui::feedback::Submission>,
    /// Who filed it.
    #[model(readonly)]
    pub author: MemberId,
    /// When, in seconds since the epoch.
    #[model(readonly)]
    pub ts: i64,
    /// What happened to it since.
    #[model(readonly)]
    pub triage: Collection<Triage>,
    /// Who answered it with which emoji (a +1 from someone who sees the
    /// same): the framework's reactions, as on a message, each the
    /// invoker's own.
    #[model(readonly)]
    pub reactions: ui::tilo_resource::Reactions,
}

/// One triage step of a feedback report.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Triage {
    #[model(readonly)]
    pub author: MemberId,
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
    /// The operations dispatched so far, each by the id minted when it was.
    #[serde(default)]
    pub trace: Vec<StepUpdate>,
    /// The run's working record of a failure, for whoever debugs it later;
    /// never the reply's words. Absent when nothing failed.
    #[serde(default)]
    pub detail: Option<String>,
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
    if !tilo_content::valid_reference(&hash) {
        return Err(format!(
            "{address:?} is not a content address: post what another step handed over"
        ));
    }
    let name = params.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 255 || name.contains(['/', '\\']) {
        return Err("a posted file has a short name without a slash".into());
    }
    let bytes =
        ui::tilo_resource::content::read_hash(&hash).map_err(|refusal| refusal.to_string())?;
    let meta = tilo_content::sniff(&bytes);
    params.content.clone_from(&hash);
    params.name.clone_from(&name);
    params.attachment = Some(
        if meta.mime.starts_with("image/") && meta.w > 0 && meta.h > 0 {
            tilo_reference::Attached::Image {
                name,
                blob: hash,
                w: meta.w,
                h: meta.h,
            }
        } else {
            tilo_reference::Attached::File {
                name,
                blob: hash,
                size: meta.len,
            }
        },
    );
    Ok(())
}

/// Who an action is by; refused without one, since a report or a read mark
/// is always someone's.
pub(crate) fn author(cx: &ActionCx) -> Result<MemberId, Refusal> {
    cx.author()
        .cloned()
        .ok_or_else(|| Refusal::new("an action in chat is always someone's"))
}

pub(crate) fn seconds(cx: &ActionCx) -> i64 {
    cx.now_ms().div_euclid(1000)
}

/// A portable address, never copied content, a local path or an access grant.
///
/// Its kind may be empty when its id is a resource's published address
/// (`tl:<app>:<id>`, as a tilolang app publishes one): the address names the
/// resource and the catalogue its kind, exactly as [`tilo_reference::Embed`]
/// reads one, so a reference to a resource no catalogue has loaded yet still
/// attaches and resolves where it is read. Any other id names its kind.
pub(crate) fn valid_reference(reference: &tilo_reference::ResourceRef) -> bool {
    let field = |value: &str, max: usize| {
        !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
    };
    let published = |id: &str| {
        // `tl:<app>:<id>`: the app is dotted and never holds a `:`, so the address says its app
        // and its published id, and its kind is whatever the catalogue resolves it to.
        id.strip_prefix("tl:")
            .and_then(|rest| rest.split_once(':'))
            .is_some_and(|(app, id)| !app.is_empty() && !id.is_empty())
    };
    // A named kind is bounded like any other address; an empty one is only a resource's
    // published address, the form the catalogue reads without a kind.
    let kind = if reference.kind.is_empty() {
        published(&reference.id)
    } else {
        field(&reference.kind, 128)
            && reference
                .kind
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    kind && field(&reference.id, 2048)
        && !reference.id.starts_with(['/', '\\'])
        && !reference.id.contains("://")
        && field(&reference.label, 512)
        && reference.span.is_none_or(|span| span.start <= span.end)
}

/// What a message may carry: a bounded address or part of one, words worth
/// reading, or a file this workspace's store holds.
pub(crate) fn valid_attached(attached: &tilo_reference::Attached) -> bool {
    use tilo_reference::Attached;
    let valid_name = |name: &str| {
        !name.is_empty()
            && name.len() <= MAX_ATTACHMENT_NAME_BYTES
            && !name.chars().any(char::is_control)
    };
    let bytes = |size: u64| (1..=MAX_ATTACHMENT_BYTES).contains(&size);
    match attached {
        Attached::Resource { at, anchor, label } => {
            valid_resource_at(at) && valid_anchor(anchor.as_ref()) && label.len() <= 512
        }
        Attached::Text { text, from } => {
            !text.trim().is_empty()
                && text.len() <= MAX_MESSAGE_TEXT_BYTES
                && from.as_ref().is_none_or(valid_resource_at)
        }
        Attached::Image { name, blob, w, h } => {
            valid_name(name)
                && tilo_content::valid_reference(blob)
                && (1..=MAX_ATTACHMENT_DIMENSION).contains(w)
                && (1..=MAX_ATTACHMENT_DIMENSION).contains(h)
        }
        Attached::Video {
            name,
            blob,
            w,
            h,
            size,
        } => {
            valid_name(name)
                && tilo_content::valid_reference(blob)
                && bytes(*size)
                && *w <= MAX_ATTACHMENT_DIMENSION
                && *h <= MAX_ATTACHMENT_DIMENSION
        }
        Attached::File { name, blob, size } => {
            valid_name(name) && tilo_content::valid_reference(blob) && bytes(*size)
        }
    }
}

/// The ids of the items an anchor names, and the path and cursors of a text
/// run: the item ids an app publishes, which are bounded like a place's.
fn valid_anchor(anchor: Option<&tilo_reference::Anchor>) -> bool {
    use tilo_reference::Anchor;
    let item = |id: &str| !id.is_empty() && id.len() <= 2048 && !id.chars().any(char::is_control);
    match anchor {
        None => true,
        Some(Anchor::Item { id }) => item(id),
        Some(Anchor::Items { ids }) => {
            !ids.is_empty() && ids.len() <= 256 && ids.iter().all(|id| item(id))
        }
        Some(Anchor::Text { path, start, end }) => {
            path.len() <= 2048
                && ui::automerge::Cursor::try_from(start.as_str()).is_ok()
                && ui::automerge::Cursor::try_from(end.as_str()).is_ok()
        }
    }
}

/// An existing record of an existing app, and the resource inside it. The
/// host resolves and authorizes it; storing this address neither admits a
/// member nor grants Read.
pub(crate) fn valid_resource_at(at: &tilo_reference::ResourceAt) -> bool {
    let field = |value: &str| {
        !value.is_empty()
            && value.len() <= 2048
            && !value.chars().any(char::is_control)
            && !value.contains("://")
            && !value.contains('/')
            && !value.contains('\\')
    };
    field(&at.app) && at.record.as_deref().is_none_or(field) && valid_reference(&at.reference)
}

/// The id of `agent`'s reply to the message `summons`: the same on every
/// replica and for every record of its work, so the agent never needs to be
/// told the id of what it wrote.
///
/// It is the reply of the first attempt. A message a person asked again
/// (`retry`) is answered in a reply of its own ([`reply_id_at`]), never in the
/// one that failed.
#[must_use]
pub fn reply_id(summons: &Id, agent: &MemberId) -> Id {
    reply_id_at(summons, agent, 0)
}

/// The id of `agent`'s reply to `summons` on attempt `attempt` (`0` the first,
/// [`Message::attempts`] the one being written now): derived the same way on
/// every replica, so an agent never needs to be told the id of what it wrote,
/// and a retried turn never overwrites the reply that failed.
#[must_use]
pub fn reply_id_at(summons: &Id, agent: &MemberId, attempt: u32) -> Id {
    use std::hash::Hasher as _;
    let mut hash = fnv::FnvHasher::default();
    hash.write(summons.as_str().as_bytes());
    hash.write(&[0]);
    hash.write(&agent.actor_bytes());
    if attempt > 0 {
        hash.write(&[0]);
        hash.write(&attempt.to_le_bytes());
    }
    Id::from_raw(format!("m_reply_{:016x}", hash.finish()))
}

/// A new message's fields beyond its channel, author and time.
#[derive(Default)]
pub(crate) struct Posting {
    pub text: String,
    pub body: Vec<ClipBlock>,
    pub parent: Option<Id>,
    /// The run of the message it answers that it quotes, when it answers a span.
    pub quoted: Option<Quote>,
    /// What it carries besides its words.
    pub attachments: Vec<tilo_reference::Attached>,
    pub appearance: Option<tilo_actions::Appearance>,
    pub share: Option<WindowShare>,
    /// What it asks one person.
    pub question: Option<crate::question::Question>,
    /// Who the text refers to, as the chat record resolved it where the
    /// message was written.
    pub mentions: Vec<crate::mention::Mention>,
    /// Where it was first written, when it was moved here from elsewhere.
    pub origin: Option<ui::tilo_resource::Origin>,
}

impl Chat {
    /// The id of the one conversation with exactly `members`: derived
    /// from the set, so two devices that open the same conversation at once
    /// make one, and neither has to create it first.
    #[must_use]
    pub fn direct_id(members: impl IntoIterator<Item = MemberId>) -> Id {
        let mut members: Vec<MemberId> = members.into_iter().collect();
        members.sort();
        members.dedup();
        let members = serde_json::to_string(&members).unwrap_or_default();
        Id::from_raw(format!("{DIRECT_CONVERSATION_PREFIX}{members}"))
    }

    /// The members the direct conversation `id` names, or none when
    /// `id` names no direct conversation.
    #[must_use]
    pub fn direct_members(id: &Id) -> Option<Vec<MemberId>> {
        let members = id.as_str().strip_prefix(DIRECT_CONVERSATION_PREFIX)?;
        serde_json::from_str::<Vec<MemberId>>(members)
            .ok()
            .filter(|members| !members.is_empty())
    }

    /// Whether `id` names a direct conversation.
    #[must_use]
    pub fn is_direct(id: &Id) -> bool {
        id.as_str().starts_with(DIRECT_CONVERSATION_PREFIX)
    }

    /// The conversation `id` names: a channel the record keeps, or the
    /// direct conversation with the members its id names. `None` for an
    /// id that names neither.
    #[must_use]
    pub fn conversation(&self, id: &Id) -> Option<Conversation> {
        if let Some(channel) = self.channels.get(id) {
            return Some(channel.clone());
        }
        let members = Self::direct_members(id)?;
        Some(Conversation {
            members,
            ..Conversation::default()
        })
    }

    /// Who the conversation `id` is with: a channel of the whole workspace
    /// has no members.
    #[must_use]
    pub fn conversation_members(&self, id: &Id) -> Vec<MemberId> {
        if let Some(channel) = self.channels.get(id) {
            return channel.members.clone();
        }
        Self::direct_members(id).unwrap_or_default()
    }

    /// Whether the conversation `id` takes messages now: a text channel of
    /// the record, or a direct conversation, named by its members.
    ///
    /// A member is one by being named as one: the workspace's member list and
    /// its record of its agents say who exists, and the chat does not list
    /// them a second time, so a conversation with someone who never opened
    /// Chat takes the first message to them.
    #[must_use]
    pub fn takes_messages(&self, id: &Id) -> bool {
        match self.channels.get(id) {
            Some(channel) => channel.kind != ChannelKind::Voice,
            None => Self::direct_members(id).is_some(),
        }
    }

    /// Everyone who can be mentioned: the workspace's people and agents.
    #[must_use]
    pub fn members(&self) -> Vec<Person> {
        self.people.iter().cloned().collect()
    }

    /// Who a message refers to: the mentions marked in its paragraphs as
    /// written, or for plain text each `@Name` of a member.
    pub(crate) fn mentions(&self, text: &str, body: &[ClipBlock]) -> Vec<crate::mention::Mention> {
        if body.is_empty() {
            return crate::mention::resolve(text, &self.members());
        }
        let blocks: Vec<_> = body.iter().map(tilo_rich_text::content_of_clip).collect();
        crate::body::mentions(&blocks, &self.members())
    }

    /// Add `channel` as `id`.
    pub(crate) fn open_channel(&mut self, id: Id, channel: Conversation) {
        self.channels.push(id, channel);
    }

    /// Make channel `id` named `name`, refusing a category that is not
    /// here and a name that is only `#`. `members` is who the
    /// conversation is with, empty for a channel of the whole workspace.
    fn make_channel(
        &mut self,
        id: Id,
        name: &NonBlank,
        category: Option<Id>,
        kind: Option<ChannelKind>,
        topic: Option<String>,
        members: Vec<MemberId>,
    ) -> Result<(), Refusal> {
        if let Some(category) = &category
            && !self.categories.contains(category)
        {
            return Err(Refusal::new("there is no such category"));
        }
        let name = channel_name(name)?;
        self.open_channel(
            id,
            Conversation {
                category,
                name: name.to_owned(),
                topic: topic
                    .map(|topic| topic.trim().to_owned())
                    .unwrap_or_default(),
                kind: kind.unwrap_or_default(),
                members,
                ..Conversation::default()
            },
        );
        Ok(())
    }

    /// The person `id`, added when the workspace does not know them yet.
    fn person(&mut self, id: &MemberId) -> Result<&mut Person, Refusal> {
        let key = member_slot(id);
        if self.people.find(&key).is_none() {
            self.people.push(Person::unnamed(id.clone()));
        }
        self.people
            .find_mut(&key)
            .ok_or_else(|| Refusal::new("there is no such person"))
    }

    /// Start a fresh record: its creator, and the workspace's first channels
    /// when it has none yet ([`open_workspace`]).
    pub(crate) fn begin(
        &mut self,
        creator: &MemberId,
        (name, initials, color): (&str, &str, u32),
    ) -> Result<(), Refusal> {
        let person = self.person(creator)?;
        name.clone_into(&mut person.name);
        initials.clone_into(&mut person.initials);
        person.color = color;
        person.status = Presence::Online;
        open_workspace(&mut self.channels);
        Ok(())
    }
}

#[ui::tilo_resource::resource(
    kind = "chat.workspace",
    params = chat,
    search = Chat::search,
    read_content = Chat::content,
    effects = crate::effects::ChatEffect,
    item_views = crate::effects::item_views,
)]
impl Chat {
    /// Start the chat.
    ///
    /// Its creator, and the workspace's first channels when it has none yet.
    #[action(housekeeping, audience = "agents")]
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
    /// How you show in the workspace: your name, initials and colour, and that
    /// you are here.
    #[action(housekeeping, audience = "agents")]
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

    // The workspace's channels and categories are its layout, kept by those who
    // moderate it (as a server's channels are kept by those who may manage
    // them): a member talks in the channels, and does not make, rename,
    // move or delete them.

    /// New category.
    #[action(moderation)]
    pub fn create_category(
        &mut self,
        cx: &mut ActionCx,
        #[param("Category name")] name: NonBlank,
    ) -> Result<(), Refusal> {
        self.categories.push(
            cx.id(),
            Category {
                name: name.into_string(),
            },
        );
        Ok(())
    }

    /// New channel.
    #[action(moderation)]
    pub fn create_channel(
        &mut self,
        cx: &mut ActionCx,
        #[param("Channel name")] name: NonBlank,
        #[param("The category to list it under; none for uncategorised")] category: Option<Id>,
        #[param("text for a conversation, voice for a call; text when absent")] kind: Option<
            ChannelKind,
        >,
        #[param("What the channel is about")] topic: Option<String>,
    ) -> Result<(), Refusal> {
        let id = cx.id();
        self.make_channel(id, &name, category, kind, topic, Vec::new())
    }

    /// New channel with an id.
    ///
    /// A channel under an id of your choosing, so a later step can name it
    /// before it exists (`chat.workspace/channels/<id>`). Naming a channel
    /// that exists leaves it as it is.
    ///
    /// `members` says who the conversation is with: a conversation an
    /// agent takes part in is one whose agents answer the person's messages in
    /// it. None is a channel of the whole workspace.
    #[action(moderation, audience = "agents")]
    pub fn create_channel_with_id(
        &mut self,
        #[param("The channel's id: letters, digits, - and _, at most 64")] id: String,
        #[param("Channel name")] name: NonBlank,
        #[param("The category to list it under; none for uncategorised")] category: Option<Id>,
        #[param("text for a conversation, voice for a call; text when absent")] kind: Option<
            ChannelKind,
        >,
        #[param("What the channel is about")] topic: Option<String>,
        #[param("Who the conversation is with; none for the whole workspace")] members: Option<
            Vec<MemberId>,
        >,
    ) -> Result<(), Refusal> {
        let id = given_channel_id(&id)?;
        // A step run again, or two seeds naming one channel, meet on the
        // channel that is there.
        if self.channels.contains(&id) {
            return Ok(());
        }
        let members = members.unwrap_or_default();
        self.make_channel(id, &name, category, kind, topic, members)
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

    /// Move a category.
    ///
    /// One place up or down among the categories.
    #[action(item = categories, moderation)]
    pub fn move_category(&mut self, category: &Id, up: bool) -> Result<(), Refusal> {
        let at = self
            .categories
            .position(category)
            .ok_or_else(|| Refusal::new("there is no such category"))?;
        let target = if up {
            at.checked_sub(1)
        } else {
            Some(at.saturating_add(1)).filter(|next| *next < self.categories.len())
        };
        if let Some(target) = target {
            self.categories.move_to(category, target);
        }
        Ok(())
    }

    /// Arrange channels.
    ///
    /// Lists the categories named in this order, and the channels named in
    /// this order, each under the category it names. One not named stays
    /// right after the one it followed.
    #[action(moderation)]
    pub fn arrange_channels(
        &mut self,
        #[param("Categories, in the order to list them")] categories: Vec<String>,
        #[param("Channels, in the order to list them, each with the category to list it under")]
        channels: Vec<ChannelPlace>,
    ) -> Result<(), Refusal> {
        let category_ids: Vec<Id> = categories
            .iter()
            .map(|id| Id::from_raw(id.as_str()))
            .collect();
        let channel_ids: Vec<Id> = channels
            .iter()
            .map(|place| Id::from_raw(place.channel.as_str()))
            .collect();
        let distinct =
            |ids: &[Id]| ids.iter().collect::<std::collections::BTreeSet<_>>().len() == ids.len();
        if !distinct(&category_ids) || !distinct(&channel_ids) {
            return Err(Refusal::new("each channel and category is named once"));
        }
        if !category_ids.iter().all(|id| self.categories.contains(id))
            || channels.iter().any(|place| {
                place
                    .category
                    .as_deref()
                    .is_some_and(|category| !self.categories.contains(&Id::from_raw(category)))
            })
        {
            return Err(Refusal::new("there is no such category"));
        }
        if !channel_ids.iter().all(|id| self.channels.contains(id)) {
            return Err(Refusal::new("there is no such channel"));
        }
        for (id, place) in channel_ids.iter().zip(&channels) {
            let category = place.category.as_deref().map(Id::from_raw);
            if self
                .channels
                .get(id)
                .is_some_and(|c| c.category != category)
                && let Some(channel) = self.channels.get_mut(id)
            {
                channel.category = category;
            }
        }
        arrange(&mut self.categories, &category_ids);
        arrange(&mut self.channels, &channel_ids);
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
        #[param("The channel's new name")] name: NonBlank,
    ) -> Result<(), Refusal> {
        let name = channel_name(&name)?;
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
        #[param("The category's new name")] name: NonBlank,
    ) -> Result<(), Refusal> {
        let renamed = self
            .categories
            .get_mut(category)
            .ok_or_else(|| Refusal::new("there is no such category"))?;
        renamed.name = name.into_string();
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
    /// Tells the workspace's moderators that this message, and its author,
    /// should not be here.
    #[action(audience = "agents")]
    pub fn report_message(
        &mut self,
        cx: &mut ActionCx,
        #[param("The message")] message: String,
        #[param("Who wrote it")] author: MemberId,
        #[param("What is wrong with it")] reason: NonBlank,
    ) -> Result<(), Refusal> {
        if author.id().is_empty() || author.id().len() > 4096 {
            return Err(Refusal::new("a reported message is someone's"));
        }
        self.moderation.report(
            cx,
            author,
            Some("so.tilo.chat".to_owned()),
            None,
            Some(message),
            reason,
        )
    }

    /// Report person.
    ///
    /// Tells the workspace's moderators that this person is misbehaving.
    #[action(item = people)]
    pub fn report_person(
        &mut self,
        cx: &mut ActionCx,
        person: &Id,
        #[param("What they did")] reason: NonBlank,
    ) -> Result<(), Refusal> {
        let Some(person) = self.people.find(person) else {
            return Err(Refusal::new("there is no such person"));
        };
        let person = person.id.clone();
        self.moderation.report(cx, person, None, None, None, reason)
    }

    /// Decide on a report.
    ///
    /// Records what was done about a person and why, settling their
    /// reports; the workspace's governance and history carry it out.
    #[action(moderation, audience = "agents")]
    pub fn decide_reports(
        &mut self,
        cx: &mut ActionCx,
        #[param("A new id for the decision")] id: String,
        #[param("The person decided on")] person: MemberId,
        #[param("The reports it settles")] reports: Vec<String>,
        #[param("What is done, and why")] ruling: moderation_model::Ruling,
    ) -> Result<(), Refusal> {
        self.moderation.decide(cx, id, person, reports, ruling)
    }

    /// Overturn a decision.
    ///
    /// A Moderator's word against an earlier decision; the host lifts what
    /// it imposed and restores what it took back.
    #[action(moderation, audience = "agents")]
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
    #[action(item = channels, housekeeping, audience = "agents")]
    pub fn mark_read(
        &mut self,
        cx: &mut ActionCx,
        channel: &Id,
        #[param("The part of the chat's messages you read into")] part: u64,
        #[param("How many of the channel's top-level messages you saw in that part")] seen: u32,
    ) -> Result<(), Refusal> {
        let me = author(cx)?;
        if self.conversation(channel).is_none() {
            return Err(Refusal::new("there is no such channel"));
        }
        let read = crate::parts::ReadMark {
            part: i64::try_from(part).unwrap_or(i64::MAX),
            seen: i64::from(seen),
        };
        let marks = self.read.entry(member_slot(&me).to_string()).or_default();
        let mark = marks.entry(channel.to_string()).or_default();
        if *mark < read {
            *mark = read;
        }
        Ok(())
    }

    /// Keep a summary.
    ///
    /// What the agent answering in a conversation keeps of it, through the message it names:
    /// what its later turns are told in place of the messages before ([`crate::recall`]). Only
    /// an agent the conversation is with keeps one, under its own name, and a summary only
    /// reaches further: one that ends before the one kept changes nothing, so two runtimes
    /// answering for the same agent keep the further of the two.
    #[action(item = channels, housekeeping, audience = "agents")]
    pub fn keep_summary(
        &mut self,
        cx: &mut ActionCx,
        channel: &Id,
        #[param("The last message it takes in")] through: String,
        #[param("When that message was written, in seconds since the epoch")] through_ts: i64,
        #[param("The summary")] text: String,
    ) -> Result<(), Refusal> {
        let me = author(cx)?;
        if me.is_person() || !self.conversation_members(channel).contains(&me) {
            return Err(Refusal::new(
                "only an agent the conversation is with keeps its summary",
            ));
        }
        let text = text.trim();
        if text.is_empty() || text.len() > ui::collaboration::recall::SUMMARY_BYTES {
            return Err(Refusal::new(format!(
                "a summary is 1 to {} bytes of text",
                ui::collaboration::recall::SUMMARY_BYTES
            )));
        }
        let through = through.trim();
        if through.is_empty() || through.len() > 256 || through.chars().any(char::is_control) {
            return Err(Refusal::new("a summary names the last message it takes in"));
        }
        let kept = self
            .summaries
            .entry(channel.to_string())
            .or_default()
            .entry(member_slot(&me).to_string())
            .or_default();
        if !kept.text.is_empty() && through_ts < kept.through_ts {
            return Ok(());
        }
        text.clone_into(&mut kept.text);
        kept.through = Atom(through.to_owned());
        kept.through_ts = through_ts;
        Ok(())
    }

    /// Submit feedback.
    ///
    /// A reviewed report, filed in the channel and kept with its triage;
    /// the channel shows it among its messages.
    // Filing a report is commenting: whoever may comment in the workspace may.
    // Only a person files one: the submission is what the feedback sheet
    // reviewed with them, which an agent cannot produce, so offering it to
    // agents only drew them away from `post`.
    #[action(item = channels, field = "comments", audience = "people")]
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
            return if known.submission.as_ref() == Some(&submission) && known.author == me {
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
                author: me,
                ts,
                triage: Collection::new(),
                reactions: ui::tilo_resource::Reactions::new(),
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
                    author: me,
                    ts,
                    update: Some(update),
                },
            );
        Ok(())
    }
}

#[cfg(test)]
mod mention_tests;
#[cfg(test)]
mod terminal_tests;
#[cfg(test)]
mod tests;
