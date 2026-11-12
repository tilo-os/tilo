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
use ui::automerge::ObjId;
use ui::tilo_resource::evolution::{
    collection_items, map_at, map_values, retype_json, retype_variant_fields,
};
use ui::tilo_resource::{
    self, ActionCx, Atom, Collection, Id, Identified, JsonSchema, Keyed, Model, ModelError,
    NonBlank, Refusal,
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

/// Bring a chat record of version `from` up to this one. No step deletes
/// anything a person wrote: what the record no longer reads stays where it
/// is. Version 1 kept everyone's unsent drafts in the workspace, where every
/// member edited one shared draft per channel; drafts are each writer's own
/// now ([`crate::drafts`]), and the workspace's copy stays under `drafts`.
/// Versions 1 and 2 kept the messages in the record and read marks as
/// counts; new messages are kept in parts beside it now ([`crate::parts`])
/// and a read mark is a position in them. The record's messages stay under
/// `messages`, and the counts move to `read_counts` so `read` can hold
/// positions ([`keep_read_counts`]). Versions 1 to 3
/// kept workspaces of the chat's own, and each channel and category named
/// its workspace; the record is the chat of the workspace the window is in
/// now and reads neither, so both stay where they are, names included, and
/// every channel and category is the workspace's. Versions 1 to 4 kept a
/// filed report's submission,
/// a triage step's update and a moderation decision's reports and
/// take-backs as one JSON string each; they are model values now, rewritten
/// in place. Chats made before Chat's record was a model kept each channel's
/// and category's fields flat in its entry, and opened as version 1 with
/// those fields unread; up to version 5 they are read into the item
/// ([`restore_flat_items`]). Versions 1 to 6 stored a triage step's update
/// with its fields flat beside its variant's name; they are stored by their
/// variant now. Up to version 7 a conversation with an agent was a channel
/// carrying the agent's id (`conversation_with`); it is the participant the
/// conversation is with now ([`participants_of_old_channels`]).
///
/// # Errors
/// When `from` is not a version this chat was, or a stored value does not
/// read as its new type.
fn migrate(
    doc: &mut ui::tilo_resource::Doc,
    from: u32,
) -> Result<(), ui::tilo_resource::ModelError> {
    use ui::automerge::ROOT;
    if !(1..=7).contains(&from) {
        return Err(ui::tilo_resource::ModelError::invalid(format!(
            "no Chat migration from {from}"
        )));
    }
    if from <= 2 {
        keep_read_counts(doc)?;
    }
    restore_flat_items(doc)?;
    if from <= 7 {
        participants_of_old_channels(doc)?;
    }
    for report in collection_items(doc, &ROOT, "feedback")? {
        retype_json::<Option<ui::feedback::Submission>>(doc, &report, "submission")?;
        for step in collection_items(doc, &report, "triage")? {
            retype_json::<Option<crate::feedback::TriageUpdate>>(doc, &step, "update")?;
            retype_variant_fields::<Option<crate::feedback::TriageUpdate>>(doc, &step, "update")?;
        }
    }
    if let Some(moderation) = map_at(doc, &ROOT, "moderation")? {
        moderation_model::record::upgrade_decisions(doc, &moderation)?;
    }
    Ok(())
}

/// Move the read marks versions 1 and 2 kept as counts (person, then
/// channel, to how many messages they had read) to `read_counts`, and start
/// `read` over as positions in the parts.
fn keep_read_counts(doc: &mut ui::tilo_resource::Doc) -> Result<(), ModelError> {
    use ui::automerge::{ObjType, ROOT, ReadDoc as _, Value};
    use ui::tilo_resource::Slot;
    if let Some(read) = map_at(doc, &ROOT, "read")? {
        let counts = Slot::Key(&ROOT, "read_counts").object(doc, ObjType::Map)?;
        for person in doc.keys(&read).collect::<Vec<_>>() {
            let Some(channels) = map_at(doc, &read, &person)? else {
                continue;
            };
            let kept = Slot::Key(&counts, &person).object(doc, ObjType::Map)?;
            for channel in doc.keys(&channels).collect::<Vec<_>>() {
                if let Some((Value::Scalar(count), _)) = doc.get(&channels, channel.as_str())? {
                    Slot::Key(&kept, &channel).put_scalar(doc, count.into_owned())?;
                }
            }
        }
    }
    let read = Slot::Key(&ROOT, "read");
    read.remove(doc)?;
    read.object(doc, ObjType::Map)?;
    Ok(())
}

/// Read the fields a chat from before its record was a model kept flat in
/// each channel's and category's entry (`name`, `topic`, `category`,
/// `kind`, `order` beside the item's `k` and `v`) into the item, where
/// those chats were opened without them: a name or topic the item holds
/// empty, a category or kind it does not hold. What the item already says
/// stays, and the flat fields stay where they are. A collection whose every
/// entry kept its flat `order` is listed in that order again.
fn restore_flat_items(doc: &mut ui::tilo_resource::Doc) -> Result<(), ModelError> {
    use ui::automerge::{ObjType, ROOT, ReadDoc as _, ScalarValue, Value};
    use ui::tilo_resource::Slot;
    let flat_str =
        |doc: &ui::tilo_resource::Doc, entry: &ObjId, key: &str| match doc.get(entry, key) {
            Ok(Some((Value::Scalar(scalar), _))) => match scalar.as_ref() {
                ScalarValue::Str(text) if !text.is_empty() => Some(text.to_string()),
                _ => None,
            },
            _ => None,
        };
    for collection in ["categories", "channels"] {
        let Some(map) = map_at(doc, &ROOT, collection)? else {
            continue;
        };
        let mut ordered = Vec::new();
        let mut every_entry_ordered = true;
        for id in doc.keys(&map).collect::<Vec<_>>() {
            let Some(entry) = map_at(doc, &map, &id)? else {
                every_entry_ordered = false;
                continue;
            };
            let value = match map_at(doc, &entry, "v")? {
                Some(value) => value,
                None if doc.get(&entry, "v")?.is_none() => {
                    Slot::Key(&entry, "v").object(doc, ObjType::Map)?
                }
                None => {
                    every_entry_ordered = false;
                    continue;
                }
            };
            for key in ["name", "topic"] {
                if let Some(flat) = flat_str(doc, &entry, key)
                    && ui::tilo_resource::read_key::<String>(doc, &value, key)
                        .is_ok_and(|held| held.is_empty())
                {
                    flat.store(doc, Slot::Key(&value, key))?;
                }
            }
            if collection == "channels" {
                if let Some(flat) = flat_str(doc, &entry, "category")
                    && doc.get(&value, "category")?.is_none()
                {
                    Some(Id::from_raw(flat)).store(doc, Slot::Key(&value, "category"))?;
                }
                if let Some(flat) = flat_str(doc, &entry, "kind")
                    && doc.get(&value, "kind")?.is_none()
                {
                    let kind: ChannelKind = flat.parse().unwrap_or_default();
                    kind.store(doc, Slot::Key(&value, "kind"))?;
                }
            }
            match doc.get(&entry, "order")? {
                Some((Value::Scalar(scalar), _)) => match scalar.as_ref() {
                    ScalarValue::Int(order) => ordered.push((*order, id, entry)),
                    ScalarValue::Uint(order) => {
                        ordered.push((i64::try_from(*order).unwrap_or(i64::MAX), id, entry));
                    }
                    _ => every_entry_ordered = false,
                },
                _ => every_entry_ordered = false,
            }
        }
        if every_entry_ordered && !ordered.is_empty() {
            ordered.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            let keys = ui::tilo_resource::order::sequence(None, None, ordered.len());
            for ((_, _, entry), key) in ordered.iter().zip(keys) {
                Slot::Key(entry, "k").put_scalar(doc, ScalarValue::Str(key.into()))?;
            }
        }
    }
    Ok(())
}

/// Read the conversations versions up to 7 kept as a channel with the agent
/// it was with (`conversation_with`) into the participant the conversation is
/// with; the old field is removed. Nothing is lost with it: a conversation
/// whose participants are only an agent is listed as that agent's, not
/// among the workspace's channels.
fn participants_of_old_channels(doc: &mut ui::tilo_resource::Doc) -> Result<(), ModelError> {
    use automerge::transaction::Transactable as _;
    use ui::automerge::{ObjType, ROOT, ReadDoc as _, ScalarValue, Value};
    use ui::tilo_resource::Slot;
    for item in collection_items(doc, &ROOT, "channels")? {
        let held = match doc.get(&item, "conversation_with")? {
            Some((Value::Scalar(scalar), _)) => match scalar.as_ref() {
                ScalarValue::Str(agent) => Some(agent.to_string()),
                _ => None,
            },
            _ => None,
        };
        if let Some(agent) = held {
            let participants = Slot::Key(&item, "participants").object(doc, ObjType::List)?;
            let keys: Vec<String> = (0..doc.length(&participants))
                .filter_map(|index| match doc.get(&participants, index) {
                    Ok(Some((Value::Scalar(scalar), _))) => match scalar.as_ref() {
                        ScalarValue::Str(key) => Some(key.to_string()),
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
            let key = Participant::agent(agent).key();
            if !keys.contains(&key) {
                let at = doc.length(&participants);
                doc.insert(&participants, at, ScalarValue::Str(key.into()))?;
            }
        }
        Slot::Key(&item, "conversation_with").remove(doc)?;
    }
    Ok(())
}

/// Bring a part of the chat's messages of an older version up to this one.
/// Version 1 kept a message's paragraphs, earlier texts, mentions, reference,
/// attachment and shown name, and an agent's record of its work, as one JSON
/// string each; they are model values now, rewritten in place. Versions 1
/// and 2 stored the fields of an attachment, of where an agent's work is and
/// of what came of each operation flat, beside the variant's name; they are
/// stored by their variant now. Versions up to 3 kept a message's shared
/// resource and the record it lives in as two fields, and its file in a
/// third; the three are one list of attachments now
/// ([`upgrade_attachments`]). Versions up to 4 kept how an agent planned its
/// work beside it (`reasoning`); that is the run's own record, and it goes.
///
/// # Errors
/// When `from` is not a version a part was, or a stored value does not read
/// as its new type.
pub(crate) fn upgrade_part(doc: &mut ui::tilo_resource::Doc, from: u32) -> Result<(), ModelError> {
    if !(1..=4).contains(&from) {
        return Err(ModelError::invalid(format!(
            "no chat part migration from {from}"
        )));
    }
    for message in collection_items(doc, &ui::automerge::ROOT, "messages")? {
        upgrade_message(doc, &message)?;
    }
    Ok(())
}

/// The messages a chat record of version 1 or 2, or from before its
/// record was a model, kept in itself under `messages`, which its upgrade
/// leaves in place: read as the record would have shown them, each under
/// its own id, by its own author, at its own time, oldest first. Version 1
/// and 2 kept a [`Collection`] of messages with some values as JSON text;
/// before the model, each channel kept a list of message maps.
pub fn kept_messages(doc: &ui::tilo_resource::Doc) -> Collection<Message> {
    use ui::automerge::{ObjType, ROOT, ReadDoc as _, Value};
    use ui::tilo_resource::read_key;
    let Ok(Some(map)) = map_at(doc, &ROOT, "messages") else {
        return Collection::default();
    };
    let mut upgraded = doc.clone();
    if let Ok(items) = collection_items(&upgraded, &ROOT, "messages") {
        for message in items {
            let _ = upgrade_message(&mut upgraded, &message);
        }
    }
    let mut kept: Collection<Message> = read_key(&upgraded, &ROOT, "messages").unwrap_or_default();
    let mut listed = Vec::new();
    for channel in doc.keys(&map).collect::<Vec<_>>() {
        let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&map, channel.as_str()) else {
            continue;
        };
        for index in 0..doc.length(&list) {
            let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(&list, index) else {
                continue;
            };
            let text = |key: &str| read_key::<String>(doc, &row, key).unwrap_or_default();
            let flag = |key: &str| read_key::<bool>(doc, &row, key).unwrap_or_default();
            let id = text("id");
            if id.is_empty() {
                continue;
            }
            let parent = text("parent");
            listed.push((
                Id::from_raw(id),
                Message {
                    channel: Id::from_raw(channel.as_str()),
                    author: Atom(text("author")),
                    ts: read_key::<i64>(doc, &row, "ts").unwrap_or_default(),
                    text: text("text"),
                    edited: flag("edited"),
                    parent: (!parent.is_empty()).then(|| Id::from_raw(parent)),
                    pinned: flag("pinned"),
                    ..Message::default()
                },
            ));
        }
    }
    for (id, message) in listed {
        if !kept.contains(&id) {
            kept.push(id, message);
        }
    }
    kept.sort_by(|a, b| a.ts.cmp(&b.ts));
    kept
}

/// The names a chat from before its record was a model kept for the people
/// in it, under `users`, by participant id: what the authors of its kept
/// messages are called where the workspace no longer names them.
pub(crate) fn kept_names(doc: &ui::tilo_resource::Doc) -> BTreeMap<String, String> {
    use ui::automerge::{ROOT, ReadDoc as _};
    let Ok(Some(users)) = map_at(doc, &ROOT, "users") else {
        return BTreeMap::new();
    };
    doc.keys(&users)
        .filter_map(|id| {
            let user = map_at(doc, &users, &id).ok()??;
            let name = ui::tilo_resource::read_key::<String>(doc, &user, "name").ok()?;
            (!name.trim().is_empty()).then_some((id, name))
        })
        .collect()
}

/// [`upgrade_part`] of one message stored at `message`: each step leaves a
/// value already stored as it reads where it is.
fn upgrade_message(doc: &mut ui::tilo_resource::Doc, message: &ObjId) -> Result<(), ModelError> {
    use crate::model::{AgentStepOutcome, AgentWorkState};
    use tilo_reference::StoredFile;
    retype_json::<Vec<ClipBlock>>(doc, message, "body")?;
    retype_json::<Vec<Revision>>(doc, message, "history")?;
    retype_json::<Vec<crate::mention::Mention>>(doc, message, "mentions")?;
    retype_json::<Option<tilo_reference::ResourceRef>>(doc, message, "reference")?;
    retype_json::<Option<StoredFile>>(doc, message, "attachment")?;
    retype_json::<Option<ShownAs>>(doc, message, "appearance")?;
    retype_variant_fields::<Option<StoredFile>>(doc, message, "attachment")?;
    upgrade_attachments(doc, message)?;
    if let Some(work) = map_at(doc, message, "work")? {
        ui::tilo_resource::Slot::Key(&work, "reasoning").remove(doc)?;
        retype_json::<Option<Transition>>(doc, &work, "state")?;
        if let Some(transition) = map_at(doc, &work, "state")? {
            retype_variant_fields::<AgentWorkState>(doc, &transition, "state")?;
        }
        if let Some(stream) = map_at(doc, &work, "stream")? {
            retype_json::<Option<crate::agent_stream::AgentFinish>>(doc, &stream, "finish")?;
        }
        for (_, step) in map_values(doc, &work, "trace")? {
            retype_json::<Option<AgentStepOutcome>>(doc, &step, "outcome")?;
            retype_variant_fields::<Option<AgentStepOutcome>>(doc, &step, "outcome")?;
        }
    }
    Ok(())
}

/// Rewrite a message's shared resource and file into its attachments, and
/// drop the fields versions up to 3 kept them in.
///
/// A shared resource becomes a [`tilo_reference::Attached::Resource`] of the
/// app and record its provider named, labelled as it was labelled then, with
/// the part it named left in the reference's span. A resource whose provider
/// was never known could not be opened then either: its words are kept,
/// quoted, rather than an address nothing could resolve.
fn upgrade_attachments(
    doc: &mut ui::tilo_resource::Doc,
    message: &ObjId,
) -> Result<(), ModelError> {
    use ui::tilo_resource::{Slot, read_key, write_key};
    let reference = read_key::<Option<tilo_reference::ResourceRef>>(doc, message, "reference")?;
    let provider = read_key::<Option<Atom>>(doc, message, "source_provider")?.map(|atom| atom.0);
    let file = read_key::<Option<tilo_reference::StoredFile>>(doc, message, "attachment")?;
    if reference.is_none() && file.is_none() && provider.is_none() {
        return Ok(());
    }
    let mut attachments = read_key::<Vec<tilo_reference::Attached>>(doc, message, "attachments")?;
    if let Some(reference) = reference {
        let attached = match provider.as_deref().and_then(crate::record::provider_record) {
            Some((app, record)) => {
                let label = reference.label.clone();
                tilo_reference::Attached::resource(
                    tilo_reference::ResourceAt {
                        app: app.to_owned(),
                        record: (!record.is_empty()).then(|| record.to_owned()),
                        reference,
                    },
                    None,
                    label,
                )
            }
            None => tilo_reference::Attached::Text {
                text: reference.label,
                from: None,
            },
        };
        attachments.push(attached);
    }
    if let Some(file) = file {
        attachments.push(file.into());
    }
    write_key(doc, message, "attachments", &attachments)?;
    Slot::Key(message, "reference").remove(doc)?;
    Slot::Key(message, "source_provider").remove(doc)?;
    Slot::Key(message, "attachment").remove(doc)?;
    Ok(())
}

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
    /// participants has, named by them and never stored
    /// ([`Chat::direct_id`]).
    #[model(readonly)]
    pub channels: Collection<Conversation>,
    /// The people and agents in the workspace, by their participant id.
    pub people: Keyed<Person>,
    /// The name every member shows for an agent task, as the person hosting
    /// it labelled it.
    pub agent_names: BTreeMap<String, String>,
    /// Each person's read mark per channel: how far into the chain of
    /// message parts they have read it.
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
    /// Who the conversation is with, by [`Participant::key`]: empty for a
    /// channel of the whole workspace. Not access control — every member
    /// reads every conversation; it decides where the conversation is
    /// listed and whose unread count moves.
    #[model(readonly)]
    pub participants: Vec<Atom>,
}

impl Conversation {
    /// Whether this is one of the workspace's channels: a conversation of
    /// all its members, which has no participants of its own.
    #[must_use]
    pub fn is_channel(&self) -> bool {
        self.participants.is_empty()
    }
}

/// The prefix of a direct conversation's id: what it is with follows it
/// (`direct:person:<root>|person:<root>`, the keys in order).
pub const DIRECT_CONVERSATION_PREFIX: &str = "direct:";

/// One of the people or agents a conversation is with.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Participant {
    /// A person, by the root key of their identity.
    Person(String),
    /// An agent, by its participant id.
    Agent(String),
}

impl Participant {
    /// The person whose root key is `root`.
    #[must_use]
    pub fn person(root: impl Into<String>) -> Self {
        Self::Person(root.into())
    }

    /// The agent whose participant id is `id`.
    #[must_use]
    pub fn agent(id: impl Into<String>) -> Self {
        Self::Agent(id.into())
    }

    /// How the participant is spelled in a conversation's id and fields:
    /// `person:<root>` or `agent:<id>`.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::Person(root) => format!("person:{root}"),
            Self::Agent(id) => format!("agent:{id}"),
        }
    }

    /// The participant `key` spells, or none when it spells neither kind.
    #[must_use]
    pub fn parse(key: &str) -> Option<Self> {
        if let Some(root) = key.strip_prefix("person:") {
            return (!root.is_empty()).then(|| Self::Person(root.to_owned()));
        }
        let id = key.strip_prefix("agent:")?;
        (!id.is_empty()).then(|| Self::Agent(id.to_owned()))
    }

    /// The person's root key or the agent's id.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Person(root) => root,
            Self::Agent(id) => id,
        }
    }

    /// Whether this is an agent.
    #[must_use]
    pub fn is_agent(&self) -> bool {
        matches!(self, Self::Agent(_))
    }

    /// The participant as a conversation's fields keep it.
    #[must_use]
    pub fn atom(&self) -> Atom {
        Atom(self.key())
    }
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
    /// The window it shares live, when it was written by sharing one: the
    /// resource is the one its first resource attachment names.
    #[model(readonly)]
    pub share: Option<WindowShare>,
    /// How many times a person asked the agent again for an answer to this
    /// message (`retry`): the attempt the agent answers in now. The answer of
    /// attempt `n` is the reply [`reply_id_at`] derives, so asking again never
    /// overwrites the reply that failed — the tries stand beside one another.
    #[model(readonly)]
    pub attempts: u32,
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
            quoted: None,
            reactions: ui::tilo_resource::Reactions::new(),
            mentions: Vec::new(),
            attachments: Vec::new(),
            pinned: false,
            work: None,
            appearance: None,
            share: None,
            attempts: 0,
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
    /// Who answered it with which emoji (a +1 from someone who sees the
    /// same): the framework's reactions, as on a message, each the
    /// invoker's own.
    #[model(readonly)]
    pub reactions: ui::tilo_resource::Reactions,
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
fn author(cx: &ActionCx) -> Result<String, Refusal> {
    cx.author()
        .filter(|author| !author.is_empty() && author.len() <= 4096)
        .map(str::to_owned)
        .ok_or_else(|| Refusal::new("an action in chat is always someone's"))
}

fn seconds(cx: &ActionCx) -> i64 {
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
/// collaborator nor grants Read.
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

/// The app and record a `record:<app>:<record>` provider names, its record
/// empty for the app's primary one: `None` when it names neither.
pub(crate) fn provider_record(provider: &str) -> Option<(&str, &str)> {
    let (app, record) = provider.strip_prefix("record:")?.split_once(':')?;
    (!app.is_empty()).then_some((app, record))
}

/// The id of `agent`'s reply to the message `summons`: the same on every
/// replica and for every record of its work, so the agent never needs to be
/// told the id of what it wrote.
///
/// It is the reply of the first attempt. A message a person asked again
/// (`retry`) is answered in a reply of its own ([`reply_id_at`]), never in the
/// one that failed.
#[must_use]
pub fn reply_id(summons: &Id, agent: &str) -> Id {
    reply_id_at(summons, agent, 0)
}

/// The id of `agent`'s reply to `summons` on attempt `attempt` (`0` the first,
/// [`Message::attempts`] the one being written now): derived the same way on
/// every replica, so an agent never needs to be told the id of what it wrote,
/// and a retried turn never overwrites the reply that failed.
#[must_use]
pub fn reply_id_at(summons: &Id, agent: &str, attempt: u32) -> Id {
    use std::hash::Hasher as _;
    let mut hash = fnv::FnvHasher::default();
    hash.write(summons.as_str().as_bytes());
    hash.write(&[0]);
    hash.write(agent.as_bytes());
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
    /// Who the text refers to, as the chat record resolved it where the
    /// message was written.
    pub mentions: Vec<crate::mention::Mention>,
}

impl Chat {
    /// The id of the one conversation with exactly `participants`: derived
    /// from the set, so two devices that open the same conversation at once
    /// make one, and neither has to create it first.
    #[must_use]
    pub fn direct_id(participants: impl IntoIterator<Item = Participant>) -> Id {
        let mut keys: Vec<String> = participants.into_iter().map(|one| one.key()).collect();
        keys.sort();
        keys.dedup();
        Id::from_raw(format!("{DIRECT_CONVERSATION_PREFIX}{}", keys.join("|")))
    }

    /// The participants the direct conversation `id` names, or none when
    /// `id` names no direct conversation.
    #[must_use]
    pub fn direct_participants(id: &Id) -> Option<Vec<Participant>> {
        let keys = id.as_str().strip_prefix(DIRECT_CONVERSATION_PREFIX)?;
        let participants: Option<Vec<Participant>> =
            keys.split('|').map(Participant::parse).collect();
        participants.filter(|participants| !participants.is_empty())
    }

    /// Whether `id` names a direct conversation.
    #[must_use]
    pub fn is_direct(id: &Id) -> bool {
        id.as_str().starts_with(DIRECT_CONVERSATION_PREFIX)
    }

    /// The conversation `id` names: a channel the record keeps, or the
    /// direct conversation with the participants its id names. `None` for an
    /// id that names neither.
    #[must_use]
    pub fn conversation(&self, id: &Id) -> Option<Conversation> {
        if let Some(channel) = self.channels.get(id) {
            return Some(channel.clone());
        }
        let participants = Self::direct_participants(id)?;
        Some(Conversation {
            participants: participants.into_iter().map(|one| one.atom()).collect(),
            ..Conversation::default()
        })
    }

    /// Who the conversation `id` is with: a channel of the whole workspace
    /// has no participants.
    #[must_use]
    pub fn conversation_participants(&self, id: &Id) -> Vec<Participant> {
        if let Some(channel) = self.channels.get(id) {
            return channel
                .participants
                .iter()
                .filter_map(|key| Participant::parse(&key.0))
                .collect();
        }
        Self::direct_participants(id).unwrap_or_default()
    }

    /// Whether the conversation `id` takes messages now: a text channel of
    /// the record, or a direct conversation between members it knows.
    ///
    /// An agent is a member by being named as one: its id is its identity,
    /// and the workspace's own record of its agents is what says it exists —
    /// the chat does not list them a second time, so a conversation with an
    /// agent the workspace just made takes the person's first message.
    #[must_use]
    pub fn takes_messages(&self, id: &Id) -> bool {
        if let Some(channel) = self.channels.get(id) {
            return channel.kind != ChannelKind::Voice;
        }
        let Some(participants) = Self::direct_participants(id) else {
            return false;
        };
        participants.iter().all(|participant| {
            participant.is_agent()
                || self
                    .people
                    .iter()
                    .any(|person| person.id.0 == participant.id())
        })
    }

    /// Everyone who can be mentioned: the workspace's people and agents.
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
    pub(crate) fn open_channel(&mut self, id: Id, channel: Conversation) {
        self.channels.push(id, channel);
    }

    /// Make channel `id` named `name`, refusing a category that is not
    /// here and a name that is only `#`. `participants` is who the
    /// conversation is with, empty for a channel of the whole workspace.
    fn make_channel(
        &mut self,
        id: Id,
        name: &NonBlank,
        category: Option<Id>,
        kind: Option<ChannelKind>,
        topic: Option<String>,
        participants: Vec<Atom>,
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
                participants,
                ..Conversation::default()
            },
        );
        Ok(())
    }

    /// The person `id`, added when the workspace does not know them yet.
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

    /// Start a fresh record: its creator, and the workspace's first channels
    /// when it has none yet ([`open_workspace`]).
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
        open_workspace(&mut self.channels);
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
    version = 8,
    migrate = migrate,
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

    /// Name an agent.
    ///
    /// The name every member shows for an agent task, or for an agent by the
    /// name the device that admitted it gave it: an agent names itself only
    /// so.
    #[action(audience = "agents")]
    pub fn name_agent(
        &mut self,
        cx: &mut ActionCx,
        participant: String,
        name: NonBlank,
    ) -> Result<(), Refusal> {
        let own = cx.author() == Some(participant.as_str())
            && cx.agent_name().as_deref() == Some(name.as_str());
        if !(participant.starts_with(tilo_actions::AGENT_TASK_PARTICIPANT_PREFIX) || own)
            || name.len() > MAX_AGENT_NAME_BYTES
        {
            return Err(Refusal::new(
                "only an agent task, or an agent by its own name, is named, with a short name",
            ));
        }
        self.agent_names.insert(participant, name.into_string());
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
    /// `participants` says who the conversation is with, by
    /// [`Participant::key`] (`person:<root>`, `agent:<id>`): a conversation an
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
        #[param("Who the conversation is with, by participant key; none for the whole workspace")]
        participants: Option<Vec<String>>,
    ) -> Result<(), Refusal> {
        let id = given_channel_id(&id)?;
        // A step run again, or two seeds naming one channel, meet on the
        // channel that is there.
        if self.channels.contains(&id) {
            return Ok(());
        }
        let participants = participants
            .unwrap_or_default()
            .iter()
            .map(|key| {
                Participant::parse(key.trim())
                    .map(|participant| participant.atom())
                    .ok_or_else(|| {
                        Refusal::new(format!("`{key}` names no person or agent of the workspace"))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.make_channel(id, &name, category, kind, topic, participants)
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
        #[param("Who wrote it")] author: String,
        #[param("What is wrong with it")] reason: NonBlank,
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
    /// Tells the workspace's moderators that this person is misbehaving.
    #[action(item = people)]
    pub fn report_person(
        &mut self,
        cx: &mut ActionCx,
        person: &Id,
        #[param("What they did")] reason: NonBlank,
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
    /// reports; the workspace's governance and history carry it out.
    #[action(moderation, audience = "agents")]
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
                    author: Atom(me),
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
mod migration_tests;
#[cfg(test)]
mod terminal_tests;
#[cfg(test)]
mod tests;
