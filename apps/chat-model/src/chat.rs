//! The chat resource: the workspace's categories and channels, each person's
//! read marks, reviewed feedback and moderation. Chat's resource is the chat of
//! one Tilo workspace and holds no workspaces of its own: every channel of the
//! resource is the workspace's. The channels' messages are kept in parts beside
//! it ([`crate::parts`]), so the resource stays small however long the conversation
//! runs. What someone is writing and has not sent is theirs, not the
//! workspace's: it is kept in their own document ([`crate::drafts`]).
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
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tilo_resource::items::item_resource_id;
use tilo_resource::search_index::{IndexKind, IndexPerson};
use tilo_resource::{
    self, ActionCx, Atom, Collection, Id, JsonSchema, MemberId, Model, NonBlank, Refusal,
    SearchEntry,
};

use crate::model::ChannelKind;

pub use tilo_resource::DIRECT_CONVERSATION_PREFIX;

/// What the workspace's member list calls each of its people and agents.
pub type PeopleNames = HashMap<MemberId, String>;

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
    /// removed only by the resource's actions, which keep them to those who
    /// keep the workspace's layout. A generic insert, move or removal would let
    /// anyone who may edit the workspace (a public workspace's visitor) rearrange it.
    #[model(readonly)]
    pub categories: Collection<Category>,
    /// The workspace's conversations, in sidebar order within their category:
    /// made, ordered and removed only by the resource's actions, for the same
    /// reason as [`Self::categories`]. The field keeps the name resources have
    /// always kept them by: a channel is a conversation the whole workspace
    /// has, named and listed; a direct conversation is one a set of
    /// members has, named by them and never stored
    /// ([`Chat::direct_id`]).
    #[model(readonly)]
    pub channels: Collection<Conversation>,
    /// Each person's read mark per channel, under [`member_marks`]: how far
    /// into the chain of message parts they have read it.
    pub read: BTreeMap<String, BTreeMap<String, crate::parts::ReadMark>>,
    /// Reviewed feedback filed in a channel, under its submission's id.
    pub feedback: Collection<FeedbackReport>,
    /// Reports of people and messages, and what the workspace's moderators (or
    /// its host's moderation agent) decided on them.
    pub moderation: moderation_model::Moderation,
    /// What the workspace's server keeps of the files members post, as the host
    /// says: a member's Chat refuses, in plain words, a file its server would
    /// not take, rather than posting one that never reaches it.
    #[model(readonly)]
    pub files: WorkspaceFiles,
    /// What each agent keeps of a conversation it answers in, by the conversation's id and then
    /// under the agent's [`member_marks`]: the summary its turns are told in place of the
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
    /// When that message was written, in milliseconds since the epoch.
    pub through_ts: i64,
}

/// What a workspace's server keeps of the files its members post; zero where it
/// sets no bound (a chat no server keeps files for).
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
    /// Archived channels leave the sidebar and stay in the resource: changed
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
/// A reviewed feedback report filed in a channel.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct FeedbackReport {
    /// What was submitted, as the person reviewed it.
    #[model(readonly)]
    pub submission: Option<tilo_feedback::Submission>,
    /// Who filed it.
    #[model(readonly)]
    pub author: MemberId,
    /// When it was filed, in milliseconds since the epoch: it stands among the
    /// channel's messages by it.
    #[model(readonly)]
    pub ts: i64,
    /// What happened to it since.
    #[model(readonly)]
    pub triage: Collection<Triage>,
    /// Who answered it with which emoji (a +1 from someone who sees the
    /// same): the framework's reactions, as on a message, each the
    /// invoker's own.
    #[model(readonly)]
    pub reactions: tilo_resource::Reactions,
}

/// One triage step of a feedback report.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Triage {
    #[model(readonly)]
    pub author: MemberId,
    /// When it was triaged, in milliseconds since the epoch.
    #[model(readonly)]
    pub ts: i64,
    #[model(readonly)]
    pub update: Option<crate::feedback::TriageUpdate>,
}
/// The key the resource keeps `member`'s read marks under.
#[must_use]
pub fn member_marks(member: &MemberId) -> Id {
    Id::from_raw(tilo_resource::text::author_mark(member))
}
/// Who an action is by; refused without one, since a report or a read mark
/// is always someone's.
pub fn author(cx: &ActionCx) -> Result<MemberId, Refusal> {
    cx.author()
        .cloned()
        .ok_or_else(|| Refusal::new("an action in chat is always someone's"))
}

impl Chat {
    /// The id of the one conversation with exactly `members`: derived
    /// from the set, so two devices that open the same conversation at once
    /// make one, and neither has to create it first.
    #[must_use]
    pub fn direct_id(members: impl IntoIterator<Item = MemberId>) -> Id {
        Id::from_raw(MemberId::direct_conversation(members))
    }

    /// The members the direct conversation `id` names, or none when
    /// `id` names no direct conversation.
    #[must_use]
    pub fn direct_members(id: &Id) -> Option<Vec<MemberId>> {
        MemberId::direct_conversation_members(id.as_str())
    }

    /// Whether `id` names a direct conversation.
    #[must_use]
    pub fn is_direct(id: &Id) -> bool {
        id.as_str().starts_with(DIRECT_CONVERSATION_PREFIX)
    }

    /// The conversation `id` names: a channel the resource keeps, or the
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
    /// the resource, or a direct conversation, named by its members.
    ///
    /// A member is one by being named as one: the workspace's member list says
    /// who exists, its agents among them, and the chat does not list them a
    /// second time, so a conversation with someone who never opened
    /// Chat takes the first message to them.
    #[must_use]
    pub fn takes_messages(&self, id: &Id) -> bool {
        match self.channels.get(id) {
            Some(channel) => channel.kind != ChannelKind::Voice,
            None => Self::direct_members(id).is_some(),
        }
    }
    /// Add `channel` as `id`.
    pub fn open_channel(&mut self, id: Id, channel: Conversation) {
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

    /// Start a fresh resource: the workspace's first channels when it has none
    /// yet ([`open_workspace`]).
    pub(crate) fn begin(&mut self) {
        open_workspace(&mut self.channels);
    }
}

#[tilo_resource::resource(
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
    /// The workspace's first channels, when it has none yet.
    #[action(housekeeping, audience = "agents")]
    pub fn begin_chat(&mut self, cx: &mut ActionCx) -> Result<(), Refusal> {
        author(cx)?;
        self.begin();
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
        // A step made again, or two seeds naming one channel, meet on the
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
    #[action]
    pub fn report_person(
        &mut self,
        cx: &mut ActionCx,
        #[param("The person")] person: MemberId,
        #[param("What they did")] reason: NonBlank,
    ) -> Result<(), Refusal> {
        if !person.is_person() {
            return Err(Refusal::new("there is no such person"));
        }
        self.moderation.report(cx, person, None, None, None, reason)
    }

    /// Decide on a report.
    ///
    /// Resources what was done about a person and why, settling their
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
    /// A Moderator's word against an earlier decision; the server lifts what
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
        let marks = self.read.entry(member_marks(&me).to_string()).or_default();
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
    ///
    /// The conversation is a plain argument and comes last: a conversation with an agent is no
    /// channel this resource lists, so the verb names no item of it.
    #[action(housekeeping, audience = "agents")]
    pub fn keep_summary(
        &mut self,
        cx: &mut ActionCx,
        #[param("The last message it takes in")] through: String,
        #[param("When that message was written, in milliseconds since the epoch")] through_ts: i64,
        #[param("The summary")] text: String,
        #[param("The conversation it is the summary of")] conversation: Id,
    ) -> Result<(), Refusal> {
        let me = author(cx)?;
        if me.is_person() || !self.conversation_members(&conversation).contains(&me) {
            return Err(Refusal::new(
                "only an agent the conversation is with keeps its summary",
            ));
        }
        let text = text.trim();
        if text.is_empty() || text.len() > tilo_actions::RECALL_SUMMARY_BYTES {
            return Err(Refusal::new(format!(
                "a summary is 1 to {} bytes of text",
                tilo_actions::RECALL_SUMMARY_BYTES
            )));
        }
        let through = through.trim();
        if through.is_empty() || through.len() > 256 || through.chars().any(char::is_control) {
            return Err(Refusal::new("a summary names the last message it takes in"));
        }
        let kept = self
            .summaries
            .entry(conversation.to_string())
            .or_default()
            .entry(member_marks(&me).to_string())
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
        let submission: tilo_feedback::Submission = serde_json::from_value(submission)
            .map_err(|_| Refusal::new("a reviewed feedback submission is required"))?;
        if submission.destination.channel != channel.as_str() {
            return Err(Refusal::new("the submission is for another channel"));
        }
        submission
            .validate()
            .map_err(|error| Refusal::new(error.to_string()))?;
        let me = author(cx)?;
        let ts = cx.now_ms();
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
                reactions: tilo_resource::Reactions::new(),
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
        let ts = cx.now_ms();
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

/// The glyph a conversation leads with when the row has no face of its own.
const CONVERSATION_ICON: &str = "message-circle";

impl Chat {
    /// What search finds in the chat resource: the workspace's channels,
    /// conversations, categories, reviewed reports and people, each opening
    /// the item it names.
    ///
    /// The messages are the messages' own document to offer
    /// ([`crate::messages::ChatPart::search`]): they are kept in parts beside
    /// this resource, and a device that never opened Chat finds them there.
    #[must_use]
    pub fn search(&self) -> Vec<SearchEntry> {
        let anchor = |field: &str, id: &Id| item_resource_id::<Self>(field, id);
        let channels = self
            .channels
            .iter()
            .filter(|(_, channel)| channel.is_channel())
            .map(|(id, channel)| conversation_entry(id, &channel.name, &channel.topic));
        // A conversation the resource holds as an item: a group of people, or a person and an
        // agent. What it is called is its own name, which a kept one is made with, else what its
        // members are called; the row shows those members beside it.
        let conversations = self
            .channels
            .iter()
            .filter(|(_, conversation)| !conversation.is_channel())
            .map(|(id, _)| {
                conversation_entry(id, &self.display_names(id, None, &HashMap::new()), "")
                    .showing(self.member_rows(id))
            });
        let categories = self.categories.iter().map(|(id, category)| {
            SearchEntry::new(&category.name, "Category").anchored(anchor("categories", id))
        });
        // A report is found by what its message says: the words its reporter
        // wrote, picked and noted, and the picks of one filed without words.
        let feedback = self.feedback.iter().filter_map(|(id, report)| {
            let submission = report.submission.as_ref()?;
            let words = submission.feedback.words();
            (!words.trim().is_empty())
                .then(|| SearchEntry::new(&words, "Feedback").anchored(anchor("feedback", id)))
        });
        channels
            .chain(conversations)
            .chain(categories)
            .chain(feedback)
            .collect()
    }

    /// The people in the conversation `id`, as a row shows them: named by the
    /// workspace's member list ([`tilo_resource::members`]), else by what their
    /// own entry is called.
    fn member_rows(&self, id: &Id) -> Vec<IndexPerson> {
        self.conversation_members(id)
            .into_iter()
            .map(|member| IndexPerson {
                name: tilo_resource::members::name_of(&member).unwrap_or_default(),
                anchor: String::new(),
                identity: member.id().to_owned(),
                agent: !member.is_person(),
            })
            .collect()
    }

    /// What the chat resource holds, for a caller that reads it: with no item,
    /// the workspace's channels; with a channel, what it is about. Its messages
    /// are kept in parts beside the resource: a caller reads them from the
    /// chain the window publishes, or through the resource api.
    pub fn content(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
        let Some(item) = item else {
            return Ok(serde_json::json!({
                "channels": self.channels.iter()
                    .filter(|(_, channel)| !channel.archived && channel.is_channel())
                    .map(|(id, channel)| serde_json::json!({
                        "id": id,
                        "name": channel.name,
                        "topic": channel.topic,
                        "voice": channel.kind == ChannelKind::Voice,
                    }))
                    .collect::<Vec<_>>(),
            }));
        };
        let (field, id) = tilo_resource::items::item_of_resource::<Self>(item.as_str())
            .unwrap_or_else(|| ("", item.clone()));
        let channel = self
            .channels
            .get(&id)
            .filter(|_| field != "messages")
            .ok_or_else(|| format!("there is no `{item}` in this chat"))?;
        Ok(serde_json::json!({
            "id": id,
            "name": channel.name,
            "topic": channel.topic,
        }))
    }

    /// What the conversation is called: its own name when it has one, and
    /// else the names of the people it is with, `me`'s left out when they
    /// are one of them and someone else is. A person the workspace has no
    /// name for goes by their tag, never blank.
    ///
    /// The one place a conversation is named, for the window and for whatever
    /// else names it (an agent's work on a turn in it), and for the row search
    /// shows.
    ///
    /// A member is named by what the member list calls them (`people`).
    #[must_use]
    pub fn display_names(&self, id: &Id, me: Option<&MemberId>, people: &PeopleNames) -> String {
        if let Some(conversation) = self.channels.get(id)
            && !conversation.members.is_empty()
            && !conversation.name.trim().is_empty()
        {
            return conversation.name.trim().to_owned();
        }
        let members = self.conversation_members(id);
        let named: Vec<String> = members
            .iter()
            .map(|member| {
                people
                    .get(member)
                    .map(|name| name.trim().to_owned())
                    .filter(|name| !name.is_empty())
                    .or_else(|| tilo_resource::members::name_of(member))
                    .unwrap_or_else(|| unnamed_label(member))
            })
            .collect();
        let others: Vec<String> = members
            .iter()
            .zip(named.iter())
            .filter(|(member, _)| Some(*member) != me)
            .map(|(_, name)| name.clone())
            .collect();
        let shown = if others.is_empty() { named } else { others };
        shown.join(", ")
    }
}

/// The row that opens conversation `id`, as the window names it: its name and topic, among the
/// resource's conversations. The resource's own search and the window's entries (a direct conversation
/// the resource keeps no row for) are one row, so a hit opens the conversation by one anchor.
#[must_use]
pub fn conversation_entry(id: &Id, name: &str, topic: &str) -> SearchEntry {
    SearchEntry::new(name.to_owned(), topic.to_owned())
        .anchored(item_resource_id::<Chat>("channels", id))
        .of_kind(IndexKind::Conversation)
        .with_icon(CONVERSATION_ICON)
}

/// Distinguish unnamed members without exposing their complete routing id.
#[must_use]
pub fn unnamed_label(id: &MemberId) -> String {
    tilo_resource::word!("chat-person-unnamed", tag = member_tag(id)).read()
}

/// What names a member whose workspace has no name for them: the end of their
/// routing id, which is theirs alone, and the whole id when it has no end.
pub fn member_tag(member: &MemberId) -> String {
    let id = member.id();
    let body = id.rsplit(['@', ':']).next().unwrap_or(id);
    let tail: String = body
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if tail.is_empty() {
        id.to_string()
    } else {
        tail
    }
}
