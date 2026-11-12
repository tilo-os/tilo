//! The Tilo side as the planners read it: Chat's records as the record door
//! reads them ([`ChatRead`]), and the workspace's member list. Read from JSON
//! rather than Chat's types so the connector does not carry Chat's code.

use serde_json::Value;

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

/// One reaction, as Chat's `Reactions` field holds it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reaction {
    /// A Unicode emoji, or a workspace emoji as `:name:`.
    pub emoji: String,
    /// The participant who reacted: a person, or the bot reacting for
    /// someone elsewhere.
    pub by: String,
    /// The outside account a bot reacted for (`discord/80351110224678912`).
    pub persona: Option<String>,
    /// The name that account shows.
    pub name: Option<String>,
}

/// A Chat attachment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attachment {
    pub blob: String,
    pub name: String,
    /// Bytes, when Chat knows them (images do not say).
    pub size: Option<u64>,
}

/// The record a message shares, and the part of it: the address Chat
/// keeps, never its content.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reference {
    pub kind: String,
    pub id: String,
    pub label: String,
    /// The record it lives in (`record:<app>:<record>`), its record empty
    /// for the app's own one.
    pub provider: Option<String>,
}

/// A window shared live into a message (`Message.share`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Share {
    /// `view` or `edit`.
    pub access: String,
    /// Whether its sharer still has it open.
    pub open: bool,
    /// Whether its sharer stopped sharing it, or it is gone.
    pub ended: bool,
}

/// A Chat message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub channel: String,
    pub author: String,
    pub text: String,
    pub parent: Option<String>,
    pub reactions: Vec<Reaction>,
    /// `(user, byte start, byte len)` of each mention in `text`.
    pub mentions: Vec<(String, usize, usize)>,
    /// The files it carries, which go across as Discord attachments.
    pub attachments: Vec<Attachment>,
    /// The name the message shows instead of its author's, when a bot posted
    /// it for someone (`Message.appearance`).
    pub appearance_name: Option<String>,
    /// The record it shares, when it was written by sharing something.
    pub reference: Option<Reference>,
    /// The live share of a window it carries.
    pub share: Option<Share>,
}

/// A person or agent in the chat.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Person {
    pub id: String,
    pub name: String,
    pub agent: bool,
    /// Their avatar's content address.
    pub avatar: Option<String>,
}

/// The workspace's chat: its categories and channels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceChat {
    pub categories: Vec<Category>,
    pub channels: Vec<Channel>,
    /// The channels holding any message.
    pub busy: Vec<String>,
}

/// Chat's records as the bridge reads them through the record door: Chat's
/// record of the workspace and the messages of the parts read of its chain
/// (its newest, and the one a change was made in), in the chain's order,
/// with each filed report's message after them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChatRead {
    record: Value,
    messages: Vec<(String, Value)>,
    /// The channels holding a message in a part not read, as the parts read
    /// count them ([`ChatRead::of`]).
    earlier: std::collections::BTreeSet<String>,
}

impl ChatRead {
    /// The door's readings `{"chat", "changed", "messages"}`: Chat's record
    /// of the workspace, the part of its messages a change was made in
    /// (`null` for none), and the chain's head, its newest part, each
    /// `{revision, records: [{record, value}]}`. A part still arriving or
    /// absent reads as holding nothing. However long the chat, a pass reads
    /// at most two parts: each part counts the messages of every channel in
    /// the parts before it, which tells the channels that hold any.
    #[must_use]
    pub fn of(read: &Value) -> Self {
        let values = |reading: Option<&Value>| -> Vec<Value> {
            reading
                .and_then(|reading| reading.get("records"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|record| record.get("value"))
                .filter(|value| value.is_object())
                .cloned()
                .collect()
        };
        let record = values(read.get("chat"))
            .into_iter()
            .next()
            .unwrap_or_default();
        let parts: Vec<Value> = ["changed", "messages"]
            .into_iter()
            .flat_map(|reading| values(read.get(reading)))
            .collect();
        let messages = parts
            .iter()
            .flat_map(|part| entries(part, "messages"))
            .collect();
        let earlier = parts
            .iter()
            .filter_map(|part| part.get("earlier")?.as_object())
            .flat_map(|counts| counts.values())
            .filter_map(Value::as_object)
            .flatten()
            .filter(|(_, count)| count.as_i64().is_some_and(|count| count > 0))
            .map(|(channel, _)| channel.clone())
            .collect();
        Self {
            earlier,
            ..Self::new(record, messages)
        }
    }

    /// Chat's record of the workspace and the messages its parts hold, in
    /// order.
    #[must_use]
    pub fn new(record: Value, mut messages: Vec<(String, Value)>) -> Self {
        messages.extend(reports(&record));
        Self {
            record,
            messages,
            earlier: std::collections::BTreeSet::new(),
        }
    }
}

/// The items of the collection `field` of a record, as a record keeps them
/// (`{id: {"k": order, "v": value}}`), in their order.
fn entries(value: &Value, field: &str) -> Vec<(String, Value)> {
    let mut items: Vec<(&str, &str, Value)> = value
        .get(field)
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(id, entry)| {
            let order = entry.get("k").and_then(Value::as_str).unwrap_or_default();
            (
                order,
                id.as_str(),
                entry.get("v").cloned().unwrap_or_default(),
            )
        })
        .collect();
    items.sort_by(|(a, a_id, _), (b, b_id, _)| (a, a_id).cmp(&(b, b_id)));
    items
        .into_iter()
        .map(|(_, id, value)| (id.to_owned(), value))
        .collect()
}

/// Each filed report of `record` as the message its channel shows:
/// [`crate::bridge::report_message`], by whoever filed it, its summary and
/// picks read out ([`ui::feedback_read_out`]), its masked picture as an
/// image and the reactions people answered it with.
fn reports(record: &Value) -> Vec<(String, Value)> {
    use ui::tilo_resource::Model as _;
    entries(record, "feedback")
        .into_iter()
        .filter_map(|(report, filed)| {
            let submission = ui::feedback::Submission::from_json(filed.get("submission")?).ok()?;
            let picture = submission.feedback.screenshot.as_ref().map(|picture| {
                serde_json::json!({
                    "$variant": "image",
                    "name": ui::feedback_read_out::PICTURE_NAME,
                    "blob": picture.hash.to_ascii_lowercase(),
                })
            });
            let message = serde_json::json!({
                "channel": submission.destination.channel,
                "author": filed.get("author").cloned().unwrap_or_default(),
                "text": ui::feedback_read_out::lines(&submission.feedback).join("\n"),
                "reactions": filed.get("reactions").cloned().unwrap_or_default(),
                "attachments": picture.into_iter().collect::<Vec<_>>(),
            });
            Some((crate::bridge::report_message(&report), message))
        })
        .collect()
}

fn text(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn opt_text(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn flag(value: &Value, field: &str) -> bool {
    value.get(field).and_then(Value::as_bool).unwrap_or(false)
}

/// A value the chat stores as JSON text (`json_model!`) or as itself.
fn json_field(value: &Value, field: &str) -> Value {
    match value.get(field) {
        Some(Value::String(text)) => serde_json::from_str(text).unwrap_or(Value::Null),
        Some(other) => other.clone(),
        None => Value::Null,
    }
}

impl WorkspaceChat {
    /// The categories and channels of the workspace's `chat`, in sidebar order.
    #[must_use]
    pub fn from_chat(chat: &ChatRead) -> Self {
        let categories = entries(&chat.record, "categories")
            .into_iter()
            .map(|(id, v)| Category {
                id,
                name: text(&v, "name"),
            })
            .collect();
        let channels = entries(&chat.record, "channels")
            .into_iter()
            .map(|(id, v)| Channel {
                id,
                name: text(&v, "name"),
                topic: text(&v, "topic"),
                voice: v.get("kind").and_then(Value::as_str) == Some("voice"),
                category: opt_text(&v, "category"),
                archived: flag(&v, "archived"),
            })
            .collect();
        let mut busy = chat.earlier.clone();
        busy.extend(
            chat.messages
                .iter()
                .map(|(_, message)| text(message, "channel")),
        );
        let busy = busy.into_iter().collect();
        Self {
            categories,
            channels,
            busy,
        }
    }

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

/// A message's reactions, as a record keeps them (`{key: {emoji, by,
/// persona?, name?, at}}`), in the order they were made.
fn reactions(value: &Value) -> Vec<Reaction> {
    let mut made: Vec<&Value> = match value.get("reactions") {
        Some(Value::Object(kept)) => kept.values().collect(),
        Some(Value::Array(listed)) => listed.iter().collect(),
        _ => Vec::new(),
    };
    made.sort_by_key(|r| r.get("at").and_then(Value::as_i64).unwrap_or_default());
    made.into_iter()
        .filter_map(|r| {
            Some(Reaction {
                emoji: r.get("emoji")?.as_str()?.to_owned(),
                by: text(r, "by"),
                persona: opt_text(r, "persona"),
                name: opt_text(r, "name"),
            })
        })
        .collect()
}

/// What a message carries besides its words: the files it posts, and the
/// record it shares. A data enum names its variant beside its fields
/// (`{"$variant": "image", "blob": …}`); a record from before kept serde's
/// externally tagged form (`{"Image": {…}}`) as JSON text.
fn carried(value: &Value) -> (Vec<Attachment>, Option<Reference>) {
    let mut files = Vec::new();
    let mut shared = None;
    let attached = json_field(value, "attachments");
    for item in attached.as_array().into_iter().flatten() {
        match variant(item).as_deref() {
            Some("image" | "video" | "file") => files.push(Attachment {
                blob: text(item, "blob"),
                name: text(item, "name"),
                size: item.get("size").and_then(Value::as_u64),
            }),
            Some("resource") if shared.is_none() => {
                let at = json_field(item, "at");
                let reference = json_field(&at, "reference");
                let Some(id) = opt_text(&reference, "id") else {
                    continue;
                };
                shared = Some(Reference {
                    kind: text(&reference, "kind"),
                    label: opt_text(&reference, "label").unwrap_or_else(|| id.clone()),
                    id,
                    provider: provider(&at),
                });
            }
            _ => {}
        }
    }
    (files, shared)
}

/// The `record:<app>:<record>` provider of the record `at` names, its
/// record empty for the app's own one.
fn provider(at: &Value) -> Option<String> {
    let app = opt_text(at, "app")?;
    let record = opt_text(at, "record").unwrap_or_default();
    Some(format!("record:{app}:{record}"))
}

/// A stored data enum's variant, lower case: as a plain string, as
/// `{"$variant": …}` or in serde's externally tagged form.
fn variant(value: &Value) -> Option<String> {
    match value {
        Value::String(name) => Some(name.to_ascii_lowercase()),
        Value::Object(object) => match object.get("$variant") {
            Some(Value::String(name)) => Some(name.to_ascii_lowercase()),
            _ => object.keys().next().map(|name| name.to_ascii_lowercase()),
        },
        _ => None,
    }
}

fn share(value: &Value) -> Option<Share> {
    let share = json_field(value, "share");
    share.as_object()?;
    Some(Share {
        access: share
            .get("access")
            .and_then(variant)
            .unwrap_or_else(|| "view".to_owned()),
        open: flag(&share, "open"),
        ended: flag(&share, "ended"),
    })
}

/// Every message `chat` holds, in the chain's order.
#[must_use]
pub fn messages(chat: &ChatRead) -> Vec<Message> {
    chat.messages
        .iter()
        .map(|(id, value)| read_message(id, value))
        .collect()
}

#[must_use]
pub fn message(chat: &ChatRead, id: &str) -> Option<Message> {
    let (id, value) = chat.messages.iter().find(|(m, _)| m == id)?;
    Some(read_message(id, value))
}

fn read_message(id: &str, v: &Value) -> Message {
    let id = id.to_owned();
    let mentions = match json_field(v, "mentions") {
        Value::Array(items) => items
            .iter()
            .filter_map(|m| {
                Some((
                    m.get("user")?.as_str()?.to_owned(),
                    usize::try_from(m.get("start")?.as_u64()?).ok()?,
                    usize::try_from(m.get("len")?.as_u64()?).ok()?,
                ))
            })
            .collect(),
        _ => Vec::new(),
    };
    let appearance = json_field(v, "appearance");
    let (attachments, reference) = carried(v);
    Message {
        id,
        channel: text(v, "channel"),
        author: match v.get("author") {
            Some(Value::String(author)) => author.clone(),
            Some(other) => other
                .as_object()
                .and_then(|o| o.values().next())
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            None => String::new(),
        },
        text: text(v, "text"),
        parent: opt_text(v, "parent"),
        reactions: reactions(v),
        mentions,
        attachments,
        appearance_name: opt_text(&appearance, "name"),
        reference,
        share: share(v),
    }
}

/// The person or agent `id` of the workspace's chat, as Chat's record keeps
/// its people (by their participant id).
#[must_use]
pub fn person(chat: &ChatRead, id: &str) -> Option<Person> {
    let people = chat.record.get("people")?.as_object()?;
    people.iter().find_map(|(key, p)| {
        let p = p.get("v").unwrap_or(p);
        (key == id || text(p, "id") == id).then(|| Person {
            id: id.to_owned(),
            name: text(p, "name"),
            agent: flag(p, "agent"),
            avatar: opt_text(p, "avatar"),
        })
    })
}

/// The Tilo role of `person` in the workspace's own read (`{"members":
/// [{"person", "role"}]}`), lower case.
#[must_use]
pub fn workspace_role(workspace: &Value, person: &str) -> Option<String> {
    workspace
        .get("members")?
        .as_array()?
        .iter()
        .find(|m| m.get("person").and_then(Value::as_str) == Some(person))
        .and_then(|m| m.get("role")?.as_str())
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn chat() -> ChatRead {
        ChatRead::new(
            json!({
                "categories": {
                    "k_chat": {"k": "a", "v": {"name": "chat"}}
                },
                "channels": {
                    "c_goal": {"k": "a", "v": {"category": null, "name": "goal", "topic": "", "kind": "text", "archived": false}},
                    "c_general": {"k": "b", "v": {"category": "k_chat", "name": "general", "topic": "talk", "kind": "text", "archived": false}},
                    "c_voice": {"k": "c", "v": {"category": "k_chat", "name": "voice", "topic": "", "kind": "voice", "archived": false}}
                },
                "people": {"u_ada": {"id": "u_ada", "name": "Ada", "agent": false, "avatar": "aa"}}
            }),
            vec![(
                "m1".into(),
                json!({"channel": "c_general", "author": "u_ada", "text": "hi @Bo", "parent": null,
                  "reactions": [{"emoji": "👍", "by": "u_bo", "at": 1},
                                {"emoji": "👍", "by": "bot:i", "persona": "discord/d7", "name": "Dee", "at": 2}],
                  "mentions": "[{\"user\":\"u_bo\",\"start\":3,\"len\":3,\"is_agent\":false}]",
                  "attachments": [{"$variant": "file", "name": "a.txt", "blob": "ff", "size": 5}]}),
            )],
        )
    }

    #[test]
    fn reads_the_parts_read_and_counts_the_channels_of_the_parts_before() {
        let part =
            |record: &str, value: Value| json!({"records": [{"record": record, "value": value}]});
        let message =
            |channel: &str, k: &str| json!({"k": k, "v": {"channel": channel, "text": k}});
        let read = ChatRead::of(&json!({
            "chat": part("", json!({"channels": {}})),
            "changed": part("p1", json!({"messages": {"m1": message("c_a", "a")}})),
            "messages": part("p2", json!({
                "messages": {"m2": message("c_b", "b")},
                "earlier": {"0": {"c_old": 3, "c_gone": 0}, "1": {"c_a": 1}}
            })),
        }));
        let ids: Vec<String> = messages(&read).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["m1", "m2"], "the changed part, then the newest");
        assert_eq!(
            WorkspaceChat::from_chat(&read).busy,
            ["c_a", "c_b", "c_old"],
            "a channel counted in an earlier part holds messages"
        );
    }

    #[test]
    fn reads_the_workspaces_chat_in_order() {
        let ws = WorkspaceChat::from_chat(&chat());
        assert_eq!(
            ws.categories,
            [Category {
                id: "k_chat".into(),
                name: "chat".into()
            }]
        );
        assert_eq!(ws.channels.len(), 3);
        assert!(ws.channel("c_voice").unwrap().voice);
        assert_eq!(ws.first_text_channel().unwrap().id, "c_goal");
    }

    #[test]
    fn reads_a_message_with_json_text_fields() {
        let m = message(&chat(), "m1").unwrap();
        assert_eq!(m.author, "u_ada");
        assert_eq!(m.mentions, [("u_bo".to_owned(), 3, 3)]);
        assert_eq!(m.reactions.len(), 2);
        assert_eq!(
            m.reactions[0],
            Reaction {
                emoji: "👍".into(),
                by: "u_bo".into(),
                persona: None,
                name: None,
            }
        );
        assert_eq!(m.reactions[1].persona.as_deref(), Some("discord/d7"));
        assert_eq!(m.reactions[1].name.as_deref(), Some("Dee"));
        assert_eq!(m.attachments[0].size, Some(5));
        assert_eq!(
            person(&chat(), "u_ada").unwrap().avatar.as_deref(),
            Some("aa")
        );
    }

    /// A message as the chat stores it now: its mentions, attachments and
    /// shown name as values, each attachment naming its variant.
    #[test]
    fn reads_a_message_with_model_fields() {
        let chat = crate::tilo::ChatRead::new(
            serde_json::Value::Null,
            vec![(
                "m2".into(),
                json!({"channel": "c_general", "author": "bot:i", "text": "hi @Bo", "parent": null, "reactions": [], "mentions": [{"user": "u_bo", "start": 3, "len": 3, "is_agent": false}], "attachments": [{"$variant": "file", "name": "a.txt", "blob": "ff", "size": 5}], "appearance": {"name": "Bo"}}),
            )],
        );
        let m = message(&chat, "m2").unwrap();
        assert_eq!(m.mentions, [("u_bo".to_owned(), 3, 3)]);
        let attachment = &m.attachments[0];
        assert_eq!((attachment.blob.as_str(), attachment.size), ("ff", Some(5)));
        assert_eq!(m.appearance_name.as_deref(), Some("Bo"));
    }

    /// A window shared live: its address, its record and the share's
    /// state, the access stored as a data enum or as text.
    #[test]
    fn reads_a_shared_window() {
        let chat = crate::tilo::ChatRead::new(
            serde_json::Value::Null,
            vec![
                (
                    "m3".into(),
                    json!({"channel": "c_general", "author": "u_ada", "text": "Plan", "parent": null, "attachments": [{"$variant": "resource", "label": "Plan", "at": {"app": "so.tilo.notes", "record": "plan", "reference": {"kind": "notes.page", "id": "notes.page:1", "label": "Plan", "span": null}}}], "share": {"access": {"$variant": "edit"}, "open": true, "ended": false}}),
                ),
                (
                    "m4".into(),
                    json!({"channel": "c_general", "author": "u_ada", "text": "Plan", "parent": null, "attachments": [{"$variant": "resource", "label": "Plan", "at": {"app": "so.tilo.notes", "reference": "{\"kind\":\"notes.page\",\"id\":\"notes.page:1\",\"label\":\"Plan\"}"}}], "share": {"access": "View", "open": false, "ended": true}}),
                ),
            ],
        );
        let live = message(&chat, "m3").unwrap();
        assert_eq!(
            live.reference,
            Some(Reference {
                kind: "notes.page".into(),
                id: "notes.page:1".into(),
                label: "Plan".into(),
                provider: Some("record:so.tilo.notes:plan".into()),
            })
        );
        assert_eq!(
            live.share,
            Some(Share {
                access: "edit".into(),
                open: true,
                ended: false
            })
        );
        let ended = message(&chat, "m4").unwrap();
        let reference = ended.reference.unwrap();
        assert_eq!(
            (reference.label.as_str(), reference.provider),
            ("Plan", Some("record:so.tilo.notes:".into()))
        );
        let share = ended.share.unwrap();
        assert_eq!((share.access.as_str(), share.ended), ("view", true));
        assert_eq!(message(&self::chat(), "m1").unwrap().share, None);
    }

    #[test]
    fn workspace_roles_are_lower_case() {
        let workspace = json!({"members": [{"person": "u_ada", "role": "Moderator"}]});
        assert_eq!(
            workspace_role(&workspace, "u_ada").as_deref(),
            Some("moderator")
        );
        assert_eq!(workspace_role(&workspace, "u_x"), None);
    }
}
