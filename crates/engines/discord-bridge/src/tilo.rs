//! The Tilo side as the planners read it: tl Chat's tree read ([`ChatRead`]), and the workspace's
//! member list. Read from JSON rather than Chat's types so the connector does not carry Chat's
//! code.
//!
//! The tree read is the whole app as the core read answers it: every root resource with each
//! collection expanded, each row `{"item", "kind", "label", "created", "fields"}`, references kept
//! as `{"type", "id", "label"}`. `item` is the row's published id (`tl:so.tilo.chat:<id>`), which
//! is what Chat's own actions take, and `id` inside a reference is the id Chat keeps.

use serde_json::Value;
use tilo_resource::MemberId;

/// Who reacted, and with what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reaction {
    /// A Unicode emoji, or a workspace emoji as `:name:`.
    pub emoji: String,
    /// The member who reacted: a person, an agent or the bridge.
    pub member: Option<MemberId>,
}

/// A reaction the bridge relayed onto a Chat message for someone elsewhere: its emoji, the
/// account there that gave it, and the name that place shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayedReaction {
    /// A Unicode emoji, or a workspace emoji as `:name:`.
    pub emoji: String,
    /// The service and account that gave it, `discord/3001`.
    pub persona: String,
    /// The name that place shows for them.
    pub by: String,
}

/// A file a message carries, as Chat keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attachment {
    /// The content address its bytes are kept at.
    pub blob: String,
    pub name: String,
    pub media: String,
    /// Bytes, when Chat knows them.
    pub size: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Category {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub topic: String,
    pub voice: bool,
    pub category: Option<String>,
    pub archived: bool,
}

/// A Chat message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// The message's published id (`tl:so.tilo.chat:<id>`).
    pub id: String,
    /// The conversation it was posted in: a channel, a thread or a direct conversation.
    pub channel: String,
    /// Who posted it here: the person, the agent, or the bridge that relayed it.
    pub author: MemberId,
    pub text: String,
    /// The thread root it answers in, when it was posted in a thread.
    pub parent: Option<String>,
    pub reactions: Vec<Reaction>,
    /// The reactions the bridge relayed for people elsewhere, which Chat shows under the names
    /// they go by there.
    pub relayed: Vec<RelayedReaction>,
    /// The files it carries, which go across as Discord attachments.
    pub attachments: Vec<Attachment>,
    /// Who wrote it where it was written, as that place named them, for a relayed message.
    pub by: String,
    /// Where a relayed message was written.
    pub written_in: String,
    /// When it was written there, in milliseconds since 1970; 0 for one written here.
    pub written: i64,
}

/// A person or agent in the chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    pub id: MemberId,
    pub name: String,
    pub agent: bool,
    /// The public address of their Tilo picture, where their workspace's host serves one: what
    /// a place outside Tilo fetches to show them as they are here.
    pub avatar_url: Option<String>,
}

/// The workspace's chat: its categories and channels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceChat {
    pub categories: Vec<Category>,
    pub channels: Vec<Channel>,
    /// The channels holding any message.
    pub busy: Vec<String>,
}

/// tl Chat's tree read: the workspace's conversations with every channel's messages and threads.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChatRead {
    tree: Value,
    /// The workspace's members as its host binds them ([`ChatRead::knowing`]): who wrote what the
    /// chat holds.
    members: Vec<Value>,
}

/// The app whose resources these are.
pub const APP: &str = "so.tilo.chat";
/// The published id prefix of Chat's resources.
const ID_PREFIX: &str = "tl:so.tilo.chat:";

impl ChatRead {
    /// The tree `read` answers: the whole app, its collections expanded.
    #[must_use]
    pub fn of(read: &Value) -> Self {
        Self {
            tree: read.clone(),
            members: Vec::new(),
        }
    }

    /// The same tree, read with the workspace's members as its host binds them (`{"members":
    /// [{"member", "name"}]}`).
    #[must_use]
    pub fn knowing(mut self, workspace: &Value) -> Self {
        self.members = workspace
            .get("members")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        self
    }

    /// The published id of the app's root: the sidebar's conversations.
    #[must_use]
    pub fn root_item(&self) -> Option<String> {
        self.root().map(item)
    }

    /// The kind of the conversation `id` names: a channel's, a thread's or a direct conversation's.
    #[must_use]
    pub fn conversation_kind(&self, id: &str) -> &'static str {
        let wanted = publish(id);
        let workspace = self.workspace();
        if workspace.channels.iter().any(|c| c.id == wanted) {
            if workspace.channels.iter().any(|c| c.id == wanted && c.voice) {
                "tl.so.tilo.chat.VoiceChannel"
            } else {
                "tl.so.tilo.chat.TextChannel"
            }
        } else {
            "tl.so.tilo.chat.Thread"
        }
    }

    /// The app's root record: the sidebar's conversations.
    fn root(&self) -> Option<&Value> {
        // A read answers `{"revision", "records": [{"record", "value"}]}`; a tilolang app's
        // record is its one document's value, which lists the app's documents. Each document's
        // row holds its `fields`: the sidebar's conversations are the root's.
        let has_fields = |value: &Value| value.get("fields").and_then(Value::as_object).is_some();
        self.tree
            .get("records")
            .and_then(Value::as_array)?
            .iter()
            .find_map(|record| {
                let value = record.get("value").unwrap_or(record);
                if has_fields(value) {
                    return Some(value);
                }
                value
                    .get("records")
                    .and_then(Value::as_array)?
                    .iter()
                    .find(|document| has_fields(document))
            })
    }

    /// The channels and categories of the workspace's chat, in sidebar order.
    #[must_use]
    pub fn workspace(&self) -> WorkspaceChat {
        let mut categories = Vec::new();
        let mut channels = Vec::new();
        for row in self.rows(self.root(), "channels") {
            match kind(row) {
                "tl.so.tilo.chat.Category" => {
                    let category = item(row);
                    for inner in self.rows(Some(row), "channels") {
                        channels.push(channel(inner, Some(category.clone())));
                    }
                    categories.push(Category {
                        id: category,
                        name: text(row, "name"),
                    });
                }
                "tl.so.tilo.chat.TextChannel" | "tl.so.tilo.chat.VoiceChannel" => {
                    channels.push(channel(row, None));
                }
                _ => {}
            }
        }
        let busy = channels
            .iter()
            .filter(|channel| !self.rows_of(&channel.id, "messages").is_empty())
            .map(|channel| channel.id.clone())
            .collect();
        WorkspaceChat {
            categories,
            channels,
            busy,
        }
    }

    /// Every message the chat holds, in the sidebar's order and each conversation's own.
    #[must_use]
    pub fn messages(&self) -> Vec<Message> {
        let mut all = Vec::new();
        for channel in self.workspace().channels {
            all.extend(self.conversation_messages(&channel.id, None));
            for thread in self.rows_of(&channel.id, "threads") {
                let thread_at = item(thread);
                let root = self.thread_root(&channel.id, &thread_at);
                all.extend(self.conversation_messages(&thread_at, root.as_deref()));
            }
        }
        all
    }

    /// The message `id` names, published or as Chat keeps it.
    #[must_use]
    pub fn message(&self, id: &str) -> Option<Message> {
        let wanted = publish(id);
        self.messages()
            .into_iter()
            .find(|message| message.id == wanted)
    }

    /// The messages of the conversation `conversation`, each with the thread root it answers in
    /// when it is a thread's.
    fn conversation_messages(&self, conversation: &str, parent: Option<&str>) -> Vec<Message> {
        self.rows_of(conversation, "messages")
            .into_iter()
            .filter_map(|row| Self::read_message(row, conversation, parent))
            .collect()
    }

    /// The thread rooted at the message whose `thread` names `thread`, in `channel`.
    fn thread_root(&self, channel: &str, thread: &str) -> Option<String> {
        self.rows_of(channel, "messages")
            .into_iter()
            .find(|row| {
                row.get("fields")
                    .and_then(|fields| fields.get("thread"))
                    .and_then(reference_id)
                    .is_some_and(|found| publish(&found) == thread)
            })
            .map(item)
    }

    fn read_message(row: &Value, conversation: &str, parent: Option<&str>) -> Option<Message> {
        let author = row
            .get("created")
            .and_then(|created| created.get("who"))
            .and_then(|who| who.get("actor"))
            .and_then(|actor| serde_json::from_value::<tilo_actions::Actor>(actor.clone()).ok())
            .and_then(|actor| actor.member_id().cloned())?;
        Some(Message {
            id: item(row),
            channel: conversation.to_owned(),
            author,
            text: text(row, "body"),
            parent: parent.map(str::to_owned),
            reactions: reactions(row),
            relayed: relayed_reactions(row),
            attachments: attachments(row),
            by: text(row, "by"),
            written_in: text(row, "written_in"),
            written: row
                .get("fields")
                .and_then(|fields| fields.get("written"))
                .and_then(Value::as_i64)
                .unwrap_or_default(),
        })
    }

    /// The channel `id` names, published or as Chat keeps it.
    #[must_use]
    pub fn channel(&self, id: &str) -> Option<Channel> {
        let wanted = publish(id);
        self.workspace()
            .channels
            .into_iter()
            .find(|channel| channel.id == wanted)
    }

    /// The thread whose root message is `root`: where a reply to a Chat thread is posted.
    #[must_use]
    pub fn thread_of(&self, root: &str) -> Option<String> {
        let wanted = publish(root);
        for channel in self.workspace().channels {
            for row in self.rows_of(&channel.id, "messages") {
                if item(row) != wanted {
                    continue;
                }
                if let Some(thread) = row
                    .get("fields")
                    .and_then(|fields| fields.get("thread"))
                    .and_then(reference_id)
                {
                    return Some(publish(&thread));
                }
            }
        }
        None
    }

    /// When the newest message `channel` holds was written, in milliseconds since the epoch.
    #[must_use]
    pub fn newest_in(&self, channel: &str) -> Option<i64> {
        let wanted = publish(channel);
        self.rows_of(&wanted, "messages")
            .into_iter()
            .map(|row| {
                row.get("fields")
                    .and_then(|fields| fields.get("written"))
                    .and_then(Value::as_i64)
                    .filter(|written| *written > 0)
                    .or_else(|| {
                        row.get("created")
                            .and_then(|created| created.get("at"))
                            .and_then(Value::as_str)
                            .and_then(iso_ms)
                    })
            })
            .max()
            .flatten()
    }

    /// The channel holding the thread `thread`, or `thread` itself when it is a channel.
    #[must_use]
    pub fn thread_channel(&self, thread: &str) -> Option<String> {
        let wanted = publish(thread);
        for channel in self.workspace().channels {
            if channel.id == wanted {
                return None;
            }
            if self
                .rows_of(&channel.id, "threads")
                .into_iter()
                .any(|row| item(row) == wanted)
            {
                return Some(channel.id);
            }
        }
        None
    }

    /// Whether `member` has a reaction `emoji` on the message `message`, as the chat holds it now.
    #[must_use]
    pub fn reacted(&self, message: &str, emoji: &str, member: &MemberId) -> bool {
        self.message(message).is_some_and(|message| {
            message.reactions.iter().any(|reaction| {
                crate::emoji::key(&reaction.emoji) == crate::emoji::key(emoji)
                    && reaction.member.as_ref() == Some(member)
            })
        })
    }

    /// The rows `node` holds in the collection `field`.
    fn rows<'v>(&self, node: Option<&'v Value>, field: &str) -> Vec<&'v Value> {
        let _ = self;
        node.and_then(|node| node.get("fields"))
            .and_then(|fields| fields.get(field))
            .and_then(|collection| collection.get("items"))
            .and_then(Value::as_array)
            .map(|items| items.iter().collect())
            .unwrap_or_default()
    }

    /// The collection `field` of the resource whose published id is `id`.
    fn rows_of(&self, id: &str, field: &str) -> Vec<&Value> {
        let wanted = publish(id);
        let found = Self::find_row(self.root(), &wanted);
        self.rows(found, field)
    }

    /// The row whose published id is `item`, anywhere in the tree.
    fn find_row<'v>(node: Option<&'v Value>, item_id: &str) -> Option<&'v Value> {
        let node = node?;
        if item(node) == item_id {
            return Some(node);
        }
        for field in node
            .get("fields")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|fields| fields.values())
        {
            if let Some(items) = field.get("items").and_then(Value::as_array) {
                for row in items {
                    if let Some(found) = Self::find_row(Some(row), item_id) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    /// The person or agent `id` of the workspace, as its member list names them.
    #[must_use]
    pub fn person(&self, id: &MemberId) -> Option<Person> {
        self.members.iter().find_map(|row| {
            let member: MemberId = serde_json::from_value(row.get("member")?.clone()).ok()?;
            (member == *id).then(|| Person {
                id: id.clone(),
                name: text(row, "name"),
                agent: !id.is_person(),
                avatar_url: Some(text(row, "avatar")).filter(|avatar| !avatar.is_empty()),
            })
        })
    }
}

/// A row's kind.
fn kind(row: &Value) -> &str {
    row.get("kind").and_then(Value::as_str).unwrap_or_default()
}

/// A row's published id.
fn item(row: &Value) -> String {
    row.get("item")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// `id` as the published id of Chat's resource: what the tree's `item` names it by.
pub fn published(id: &str) -> String {
    publish(id)
}

/// Chat's resource `id` of `kind` as Chat's actions take one among their arguments: a
/// reference to it.
#[must_use]
pub fn reference(kind: &str, id: &str) -> Value {
    serde_json::json!({ "app": APP, "id": publish(id), "kind": kind, "label": "" })
}

/// `id` as the published id of Chat's resource: what the tree's `item` names it by.
fn publish(id: &str) -> String {
    if id.starts_with(ID_PREFIX) {
        id.to_owned()
    } else {
        format!("{ID_PREFIX}{id}")
    }
}

/// A reference's id, as Chat keeps it.
fn reference_id(value: &Value) -> Option<String> {
    match value {
        Value::Object(reference) => reference.get("id")?.as_str().map(str::to_owned),
        Value::String(id) => Some(id.clone()),
        _ => None,
    }
}

fn text(value: &Value, field: &str) -> String {
    match value.get("fields").and_then(|fields| fields.get(field)) {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.as_str().unwrap_or_default().to_owned(),
        None => value
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    }
}

fn channel(row: &Value, category: Option<String>) -> Channel {
    Channel {
        id: item(row),
        name: text(row, "name"),
        topic: text(row, "topic"),
        voice: kind(row) == "tl.so.tilo.chat.VoiceChannel",
        category,
        archived: false,
    }
}

/// A message's reactions, as the chat keeps them: each emoji with the members who reacted.
fn reactions(row: &Value) -> Vec<Reaction> {
    let Some(Value::Object(by_emoji)) =
        row.get("fields").and_then(|fields| fields.get("reactions"))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (emoji, who) in by_emoji {
        for actor in who
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let member = actor
                .get("actor")
                .and_then(|actor| serde_json::from_value::<tilo_actions::Actor>(actor.clone()).ok())
                .and_then(|actor| actor.member_id().cloned());
            out.push(Reaction {
                emoji: emoji.clone(),
                member,
            });
        }
    }
    out
}

/// The reactions the bridge relayed onto a message for people elsewhere: each `RelayedReaction`
/// row's emoji, the account there that gave it, and the name that place shows.
fn relayed_reactions(row: &Value) -> Vec<RelayedReaction> {
    let Some(items) = row
        .get("fields")
        .and_then(|fields| fields.get("relayed_reactions"))
        .and_then(|relayed| relayed.get("items"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|entry| {
            let reaction = entry.get("fields").unwrap_or(entry);
            Some(RelayedReaction {
                emoji: reaction.get("emoji").and_then(Value::as_str)?.to_owned(),
                persona: reaction
                    .get("persona")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                by: reaction
                    .get("by")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .collect()
}

/// The files a message carries: each `Attached` row's name, media, size and content address.
fn attachments(row: &Value) -> Vec<Attachment> {
    let Some(items) = row
        .get("fields")
        .and_then(|fields| fields.get("attachments"))
        .and_then(|files| files.get("items"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|entry| {
            let file = entry.get("fields").unwrap_or(entry);
            let url = file
                .get("blob")
                .and_then(Value::as_str)
                .or_else(|| file.get("url").and_then(Value::as_str))
                .unwrap_or_default();
            Some(Attachment {
                blob: url.strip_prefix("tilo-blob://").unwrap_or(url).to_owned(),
                name: file.get("name").and_then(Value::as_str)?.to_owned(),
                media: file
                    .get("media")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                size: file
                    .get("size")
                    .and_then(Value::as_u64)
                    .filter(|size| *size > 0),
            })
        })
        .collect()
}

impl WorkspaceChat {
    #[must_use]
    pub fn channel(&self, id: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }

    /// The first text channel: where the bridge posts its notices.
    #[must_use]
    pub fn first_text_channel(&self) -> Option<&Channel> {
        self.channels.iter().find(|c| !c.voice && !c.archived)
    }
}

/// The Tilo role of `person` in the workspace's own read (`{"members":
/// [{"member", "role"}]}`), lower case.
#[must_use]
pub fn workspace_role(workspace: &Value, person: &str) -> Option<String> {
    workspace
        .get("members")?
        .as_array()?
        .iter()
        .find(|m| m.pointer("/member/person").and_then(Value::as_str) == Some(person))
        .and_then(|m| m.get("role")?.as_str())
        .map(str::to_ascii_lowercase)
}

/// An ISO-8601 UTC instant (`2026-11-22T06:58:42.123Z`) in milliseconds since the epoch.
fn iso_ms(at: &str) -> Option<i64> {
    let at = at.trim_end_matches('Z');
    let (date, time) = at.split_once('T')?;
    let mut fields = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (
        fields.next().and_then(Result::ok)?,
        fields.next().and_then(Result::ok)?,
        fields.next().and_then(Result::ok)?,
    );
    let mut clock = time.split(':');
    let (hour, minute) = (
        clock.next()?.parse::<i64>().ok()?,
        clock.next()?.parse::<i64>().ok()?,
    );
    let second = clock.next()?;
    let (second, millis) = match second.split_once('.') {
        Some((second, fraction)) => {
            let mut fraction = fraction.to_owned();
            fraction.truncate(3);
            while fraction.len() < 3 {
                fraction.push('0');
            }
            (second.parse::<i64>().ok()?, fraction.parse::<i64>().ok()?)
        }
        None => (second.parse::<i64>().ok()?, 0),
    };
    // Days from the civil date: Howard Hinnant's `days_from_civil`, in checked arithmetic: a
    // date so far from the epoch that its milliseconds do not fit is no instant here.
    let year = if month <= 2 {
        year.checked_sub(1)?
    } else {
        year
    };
    let era = if year >= 0 {
        year
    } else {
        year.checked_sub(399)?
    } / 400;
    let yoe = year.checked_sub(era.checked_mul(400)?)?;
    let mp = month.checked_add(9)? % 12;
    let doy = (153_i64.checked_mul(mp)?.checked_add(2)? / 5)
        .checked_add(day)?
        .checked_sub(1)?;
    let doe = yoe
        .checked_mul(365)?
        .checked_add(yoe / 4)?
        .checked_sub(yoe / 100)?
        .checked_add(doy)?;
    let days = era
        .checked_mul(146_097)?
        .checked_add(doe)?
        .checked_sub(719_468)?;
    let at = days
        .checked_mul(86_400_000)?
        .checked_add(hour.checked_mul(3_600_000)?)?
        .checked_add(minute.checked_mul(60_000)?)?
        .checked_add(second.checked_mul(1000)?)?
        .checked_add(millis)?;
    Some(at)
}
