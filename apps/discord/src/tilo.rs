//! The Tilo side as the planners read it: the Chat record's JSON form (a
//! collection is `[{"id", "value"}]`) of the room's categories and
//! channels, and the room's member list. Read from JSON rather than Chat's types so the
//! connector does not carry Chat's code,.

use serde_json::Value;

/// A Tilo category.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Category {
    pub id: String,
    pub name: String,
}

/// A Tilo channel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub topic: String,
    pub voice: bool,
    pub category: Option<String>,
    pub archived: bool,
}

/// One person's reaction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reaction {
    pub emoji: String,
    pub user: String,
    /// Set when the bridge reacted for a Discord user.
    pub persona: Option<String>,
}

/// A Chat attachment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attachment {
    pub blob: String,
    pub name: String,
    /// Bytes, when Chat knows them (images do not say).
    pub size: Option<u64>,
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
    pub attachment: Option<Attachment>,
    /// The name the message shows instead of its author's, when a bot posted
    /// it for someone (`Message.appearance`).
    pub appearance_name: Option<String>,
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

/// The room's chat: its categories and channels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoomChat {
    pub categories: Vec<Category>,
    pub channels: Vec<Channel>,
}

fn entries(value: &Value, field: &str) -> Vec<(String, Value)> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|entry| {
                    let id = entry.get("id")?.as_str()?.to_owned();
                    Some((id, entry.get("value").cloned().unwrap_or(Value::Null)))
                })
                .collect()
        })
        .unwrap_or_default()
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

impl RoomChat {
    /// The categories and channels of the room's `chat`, in sidebar order.
    #[must_use]
    pub fn from_chat(chat: &Value) -> Self {
        let categories = entries(chat, "categories")
            .into_iter()
            .map(|(id, v)| Category {
                id,
                name: text(&v, "name"),
            })
            .collect();
        let channels = entries(chat, "channels")
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
        Self {
            categories,
            channels,
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

fn reactions(value: &Value) -> Vec<Reaction> {
    let items = match json_field(value, "reactions") {
        Value::Array(items) => items,
        _ => Vec::new(),
    };
    items
        .into_iter()
        .map(|item| match item {
            // A `Set` of JSON-stored values keeps each as JSON text.
            Value::String(text) => serde_json::from_str(&text).unwrap_or(Value::Null),
            other => other,
        })
        .filter_map(|r| {
            Some(Reaction {
                emoji: r.get("emoji")?.as_str()?.to_owned(),
                user: text(&r, "user"),
                persona: opt_text(&r, "persona").or_else(|| {
                    r.get("persona")
                        .and_then(|p| p.get("account"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                }),
            })
        })
        .collect()
}

fn attachment(value: &Value) -> Option<Attachment> {
    let attachment = json_field(value, "attachment");
    let object = attachment.as_object()?;
    // A stored data enum names its variant beside its fields
    // (`{"$variant": "image", "blob": …}`); a record from before kept serde's
    // externally tagged form (`{"Image": {…}}`) as JSON text.
    let inner = if object.contains_key("$variant") {
        &attachment
    } else {
        object.values().next()?
    };
    Some(Attachment {
        blob: text(inner, "blob"),
        name: text(inner, "name"),
        size: inner.get("size").and_then(Value::as_u64),
    })
}

/// Every message `chat` holds, in the order the read gave them.
#[must_use]
pub fn messages(chat: &Value) -> Vec<Message> {
    entries(chat, "messages")
        .into_iter()
        .filter_map(|(id, _)| message(chat, &id))
        .collect()
}

/// The Chat message `id`, when `chat` holds it.
#[must_use]
pub fn message(chat: &Value, id: &str) -> Option<Message> {
    let (id, v) = entries(chat, "messages")
        .into_iter()
        .find(|(m, _)| m == id)?;
    let mentions = match json_field(&v, "mentions") {
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
    let appearance = json_field(&v, "appearance");
    Some(Message {
        id,
        channel: text(&v, "channel"),
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
        text: text(&v, "text"),
        parent: opt_text(&v, "parent"),
        reactions: reactions(&v),
        mentions,
        attachment: attachment(&v),
        appearance_name: opt_text(&appearance, "name"),
    })
}

/// The person `id` of `chat`.
#[must_use]
pub fn person(chat: &Value, id: &str) -> Option<Person> {
    let people = chat.get("people")?.as_array()?;
    people.iter().find_map(|p| {
        let p = p.get("value").unwrap_or(p);
        (text(p, "id") == id).then(|| Person {
            id: id.to_owned(),
            name: text(p, "name"),
            agent: flag(p, "agent"),
            avatar: opt_text(p, "avatar"),
        })
    })
}

/// The Tilo role of `person` in a `tilo.room` read (`{"members": [{"person",
/// "role"}]}`), lower case.
#[must_use]
pub fn room_role(room: &Value, person: &str) -> Option<String> {
    room.get("members")?
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

    pub(crate) fn chat() -> Value {
        json!({
            "categories": [
                {"id": "k_chat", "value": {"name": "chat"}}
            ],
            "channels": [
                {"id": "c_goal", "value": {"category": null, "name": "goal", "topic": "", "kind": "text", "archived": false}},
                {"id": "c_general", "value": {"category": "k_chat", "name": "general", "topic": "talk", "kind": "text", "archived": false}},
                {"id": "c_voice", "value": {"category": "k_chat", "name": "voice", "topic": "", "kind": "voice", "archived": false}}
            ],
            "messages": [
                {"id": "m1", "value": {"channel": "c_general", "author": "u_ada", "text": "hi @Bo", "parent": null,
                  "reactions": [{"emoji": "👍", "user": "u_bo"}, "{\"emoji\":\"👍\",\"user\":\"bot:i\",\"persona\":{\"service\":\"discord\",\"account\":\"d7\"}}"],
                  "mentions": "[{\"user\":\"u_bo\",\"start\":3,\"len\":3,\"is_agent\":false}]",
                  "attachment": "{\"File\":{\"name\":\"a.txt\",\"blob\":\"ff\",\"size\":5}}"}}
            ],
            "people": [{"id": "u_ada", "name": "Ada", "agent": false, "avatar": "aa"}]
        })
    }

    #[test]
    fn reads_the_rooms_chat_in_order() {
        let ws = RoomChat::from_chat(&chat());
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
        assert_eq!(m.reactions[1].persona.as_deref(), Some("d7"));
        assert_eq!(m.attachment.unwrap().size, Some(5));
        assert_eq!(
            person(&chat(), "u_ada").unwrap().avatar.as_deref(),
            Some("aa")
        );
    }

    /// A message as the chat stores it now: its mentions, attachment and
    /// shown name as values, the attachment naming its variant.
    #[test]
    fn reads_a_message_with_model_fields() {
        let chat = json!({
            "messages": [
                {"id": "m2", "value": {"channel": "c_general", "author": "bot:i", "text": "hi @Bo", "parent": null,
                  "reactions": [],
                  "mentions": [{"user": "u_bo", "start": 3, "len": 3, "is_agent": false}],
                  "attachment": {"$variant": "file", "name": "a.txt", "blob": "ff", "size": 5},
                  "appearance": {"name": "Bo"}}}
            ]
        });
        let m = message(&chat, "m2").unwrap();
        assert_eq!(m.mentions, [("u_bo".to_owned(), 3, 3)]);
        let attachment = m.attachment.unwrap();
        assert_eq!((attachment.blob.as_str(), attachment.size), ("ff", Some(5)));
        assert_eq!(m.appearance_name.as_deref(), Some("Bo"));
    }

    #[test]
    fn room_roles_are_lower_case() {
        let room = json!({"members": [{"person": "u_ada", "role": "Moderator"}]});
        assert_eq!(room_role(&room, "u_ada").as_deref(), Some("moderator"));
        assert_eq!(room_role(&room, "u_x"), None);
    }
}
