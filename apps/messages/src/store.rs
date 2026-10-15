//! The Messages record: conversations and the messages delivered into them
//! or written in them, held whole in the record.
//!
//! ## Why message text is in the record
//!
//! A person's own service histories (SMS, Signal, `WhatsApp`, Tilo) never
//! enter it: those stay the privileged broker's account-owned data, read
//! through [`crate::history`] on a device holding the account. What this
//! record holds is what was put into it on purpose — messages *delivered*
//! into it (a seeded scenario, an import, an integration relaying a text)
//! and messages *written* in it, queued for a person to send. The person
//! approved giving agents access to that content: an agent works on a room's
//! records only through their published operations, in a confined worker
//! with no network or account, so a conversation it is to read or answer
//! has to be in the record it holds — exactly as Mail's delivered mail is
//! (`apps/mail/src/local.rs`). Whoever the record is shared
//! with reads these messages, so nothing reaches it except through
//! `deliver-message`, `send-message` and `reply`.
//!
//! ## Nothing is sent from here
//!
//! `send-message` and `reply` only *queue* a message in its conversation:
//! no transport is reached from a record operation, least of all from an
//! agent's worker. A person sends it from a device holding the account.
//!
//! ## Deterministic addresses
//!
//! A conversation's id comes from what the caller names — the `conversation`
//! given, else the person it is with, normalised (`+1 555 0100` and
//! `+15550100` are one conversation, `Ada Lovelace` is `ada-lovelace`) — and
//! a delivered message's id from what it says, so a written step can name
//! `messages.conversation:<id>` before it runs, and delivering the same
//! message twice changes nothing.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::prelude::{Collection, Id, Model, tilo_resource};
use ui::tilo_actions::{ActionArgs, ActionParam, ChangeKind, ParamType};
use ui::tilo_resource::{
    ActionCx, ActionDecl, Delta, Doc, Fields, ModelError, Operation, Refusal, Resource, Stamped,
};

use crate::MessagesEffect;

/// The prefix of a conversation's address.
pub const CONVERSATION: &str = "messages.conversation:";
/// The prefix of a message's address.
pub const MESSAGE: &str = "messages.message:";

/// Bounds on what one message carries, so a hostile payload cannot grow the
/// record without limit.
const MAX_PARTY_CHARS: usize = 256;
const MAX_BODY_CHARS: usize = 16 * 1024;
const MAX_KEY_CHARS: usize = 64;

/// How much of a body a listing shows.
const SNIPPET_CHARS: usize = 160;
/// The most messages one read or search answers with.
const MOST_LISTED: usize = 200;

/// The record. Its conversations are keyed by their deterministic ids.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Messages {
    pub schema_version: u64,
    pub conversations: Collection<Conversation>,
}

/// One conversation with one person or number.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Conversation {
    /// Who it is with, as first named: a name or a number.
    pub with: String,
    /// Its messages, oldest first.
    pub messages: Collection<Message>,
}

/// One message.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Message {
    /// The sender's name or number; empty for one written here.
    pub from: String,
    /// The recipient, when known.
    pub to: String,
    /// The text.
    pub body: String,
    /// When it was sent, RFC 3339 in UTC.
    pub date: String,
    /// The same instant in milliseconds since the Unix epoch, which orders
    /// the conversation.
    pub at_ms: i64,
    /// Written here rather than received.
    pub outgoing: bool,
    /// Written here and waiting for a person to send it.
    pub queued: bool,
}

/// A message `deliver-message` adds, its ids and time already settled when
/// the action was built so every replica applies the same one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivery {
    pub conversation: String,
    pub id: String,
    pub from: String,
    #[serde(default)]
    pub to: String,
    pub body: String,
    pub date: String,
    pub at_ms: i64,
}

/// A message `send-message` or `reply` queues. `with` names who a new
/// conversation is with; without it the conversation must exist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queued {
    pub conversation: String,
    #[serde(default)]
    pub with: Option<String>,
    pub body: String,
}

/// The record's replayable operations. The wire form (`{"action", "args"}`)
/// is the one `#[resource]` gave `initialize-record`, so records written
/// before keep replaying.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", content = "args")]
pub enum MessagesOperation {
    /// Set the version marker.
    #[serde(rename = "initialize-record")]
    InitializeRecord {},
    #[serde(rename = "deliver-message")]
    Deliver(Delivery),
    #[serde(rename = "queue-message")]
    Queue(Queued),
}

impl Operation for MessagesOperation {
    type Record = Messages;

    fn perform(&self, record: &mut Messages, cx: &mut ActionCx) -> Result<(), Refusal> {
        match self {
            Self::InitializeRecord {} => {
                record.schema_version = 1;
                Ok(())
            }
            Self::Deliver(delivery) => deliver(record, delivery),
            Self::Queue(queued) => queue(record, queued, cx),
        }
    }

    fn snapshot(record: &Messages) -> Option<Messages> {
        Some(record.clone())
    }

    /// A delivery came from outside (a seed, an integration): content
    /// everyone reads, but nobody's undo step. A queued message is the
    /// writer's own, undone like any edit.
    fn change_kind(&self) -> ChangeKind {
        match self {
            Self::InitializeRecord {} => ChangeKind::Housekeeping,
            Self::Deliver(_) => ChangeKind::External,
            Self::Queue(_) => ChangeKind::Content,
        }
    }
}

fn deliver(record: &mut Messages, delivery: &Delivery) -> Result<(), Refusal> {
    check_message(&delivery.from, &delivery.to, &delivery.body)?;
    let key = Id::from_raw(checked_key(&delivery.conversation)?);
    let id = Id::from_raw(checked_key(&delivery.id)?);
    if !record.conversations.contains(&key) {
        record.conversations.push(
            key.clone(),
            Conversation {
                with: delivery.from.clone(),
                messages: Collection::new(),
            },
        );
    }
    let conversation = record
        .conversations
        .get_mut(&key)
        .ok_or("the conversation could not be made")?;
    // A message delivered before is left as it was.
    if conversation.messages.contains(&id) {
        return Ok(());
    }
    insert_in_order(
        &mut conversation.messages,
        id,
        Message {
            from: delivery.from.clone(),
            to: delivery.to.clone(),
            body: delivery.body.clone(),
            date: delivery.date.clone(),
            at_ms: delivery.at_ms,
            outgoing: false,
            queued: false,
        },
    );
    Ok(())
}

fn queue(record: &mut Messages, queued: &Queued, cx: &mut ActionCx) -> Result<(), Refusal> {
    let key = Id::from_raw(checked_key(&queued.conversation)?);
    check_message(queued.with.as_deref().unwrap_or_default(), "", &queued.body)?;
    if queued.body.trim().is_empty() {
        return Err("a message needs words".into());
    }
    if !record.conversations.contains(&key) {
        let with = queued
            .with
            .clone()
            .filter(|with| !with.trim().is_empty())
            .ok_or_else(|| {
                format!(
                    "no conversation `{}`: `read` lists them, or `send-message` starts one",
                    key.as_str()
                )
            })?;
        record.conversations.push(
            key.clone(),
            Conversation {
                with,
                messages: Collection::new(),
            },
        );
    }
    let conversation = record
        .conversations
        .get_mut(&key)
        .ok_or("the conversation could not be made")?;
    let at_ms = cx.now_ms();
    let message = Message {
        from: String::new(),
        to: conversation.with.clone(),
        body: queued.body.clone(),
        date: format_date(at_ms),
        at_ms,
        outgoing: true,
        queued: true,
    };
    let id = cx.id();
    insert_in_order(&mut conversation.messages, id, message);
    Ok(())
}

/// Insert `message` after every message sent no later than it, so a
/// conversation reads in time order whatever order it was delivered in.
fn insert_in_order(messages: &mut Collection<Message>, id: Id, message: Message) {
    let index = messages
        .values()
        .position(|held| held.at_ms > message.at_ms)
        .unwrap_or(messages.len());
    messages.insert(index, id, message);
}

fn check_message(from: &str, to: &str, body: &str) -> Result<(), Refusal> {
    if from.chars().count() > MAX_PARTY_CHARS || to.chars().count() > MAX_PARTY_CHARS {
        return Err(
            format!("a sender or recipient is at most {MAX_PARTY_CHARS} characters").into(),
        );
    }
    if body.chars().count() > MAX_BODY_CHARS {
        return Err(format!("a message is at most {MAX_BODY_CHARS} characters").into());
    }
    Ok(())
}

fn checked_key(key: &str) -> Result<&str, Refusal> {
    if key.is_empty()
        || key.chars().count() > MAX_KEY_CHARS
        || !key
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '-' | '_' | '+'))
    {
        return Err(format!("`{key}` is not a conversation or message id").into());
    }
    Ok(key)
}

/// The id of the conversation with `party`, or the one a caller named: a
/// number keeps its digits (and a leading `+`), anything else becomes a
/// lowercase slug. `None` when nothing of it is left.
#[must_use]
pub fn conversation_key(party: &str) -> Option<String> {
    let party = party.trim();
    let party = party.strip_prefix(CONVERSATION).unwrap_or(party).trim();
    let phone_like = party.chars().any(|ch| ch.is_ascii_digit())
        && party
            .chars()
            .all(|ch| ch.is_ascii_digit() || " +-().".contains(ch));
    let key: String = if phone_like {
        let digits: String = party.chars().filter(char::is_ascii_digit).collect();
        if party.starts_with('+') {
            format!("+{digits}")
        } else {
            digits
        }
    } else {
        let mut slug = String::new();
        for ch in party.chars().flat_map(char::to_lowercase) {
            if ch.is_alphanumeric() || ch == '_' {
                slug.push(ch);
            } else if !slug.is_empty() && !slug.ends_with('-') {
                slug.push('-');
            }
        }
        slug.trim_end_matches('-').to_owned()
    };
    let key: String = key.chars().take(MAX_KEY_CHARS).collect();
    let key = key.trim_end_matches('-').to_owned();
    (!key.is_empty() && key != "+").then_some(key)
}

/// A delivered message's id, from what it says: the same delivery always
/// names the same message. Two FNV-1a passes give 128 bits.
fn derived_id(parts: &[&str]) -> String {
    fn fnv1a(bytes: &[u8], basis: u64) -> u64 {
        bytes.iter().fold(basis, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }
    let joined = parts.join("\u{1f}");
    format!(
        "m{:016x}{:016x}",
        fnv1a(joined.as_bytes(), 0xcbf2_9ce4_8422_2325),
        fnv1a(joined.as_bytes(), 0x0102_0304_0506_0708)
    )
}

/// `text` as an instant in milliseconds: RFC 3339, or the same without a
/// zone (read as UTC).
fn parse_date(text: &str) -> Option<i64> {
    let text = text.trim();
    [text.to_owned(), format!("{text}Z"), format!("{text}:00Z")]
        .iter()
        .find_map(|candidate| chrono::DateTime::parse_from_rfc3339(candidate).ok())
        .map(|date| date.timestamp_millis())
}

/// `at_ms` as RFC 3339 in UTC, to the second.
fn format_date(at_ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at_ms)
        .map(|date| date.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

/// What `deliver-message` was given, authored at `now_ms`.
fn build_delivery(args: &ActionArgs, now_ms: i64) -> Result<Delivery, String> {
    let field = |name| args.text(name).unwrap_or_default().trim().to_owned();
    let from = field("from");
    if from.is_empty() {
        return Err("`from` names the sender: a name or a number".into());
    }
    let body = args.text("body").unwrap_or_default().trim().to_owned();
    if body.is_empty() {
        return Err("a message needs words".into());
    }
    let at_ms = match args.text("date").map(str::trim).filter(|d| !d.is_empty()) {
        Some(date) => parse_date(date)
            .ok_or_else(|| format!("`{date}` is not a date: write 2026-10-11T09:30:00Z"))?,
        None => now_ms,
    };
    let date = format_date(at_ms);
    let conversation = match args
        .text("conversation")
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        Some(named) => {
            conversation_key(named).ok_or_else(|| format!("`{named}` is not a conversation id"))?
        }
        None => conversation_key(&from)
            .ok_or_else(|| format!("`{from}` names no one a conversation can be with"))?,
    };
    let to = field("to");
    Ok(Delivery {
        id: derived_id(&[&conversation, &from, &to, &date, &body]),
        conversation,
        from,
        to,
        body,
        date,
        at_ms,
    })
}

/// The conversation an argument names, as an id.
fn named_conversation(args: &ActionArgs) -> Result<String, String> {
    let named = args.text("conversation").unwrap_or_default().trim();
    conversation_key(named).ok_or_else(|| format!("`{named}` is not a conversation id"))
}

impl Resource for Messages {
    const KIND: &'static str = "tilo.messages";
    const LABEL: &'static str = "Messages";

    type State = Self;
    type Action = Stamped<MessagesOperation>;

    fn read(doc: &Doc) -> Result<Self, ModelError> {
        <Self as Fields>::read_fields(doc, &ui::automerge::ROOT)
    }

    fn refresh(state: &mut Self, doc: &Doc, delta: &Delta) -> Result<(), ModelError> {
        <Self as Fields>::refresh_fields(state, doc, &ui::automerge::ROOT, delta)
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        <Self as Fields>::write_fields(&Self::default(), doc, &ui::automerge::ROOT)
    }

    fn shape() -> Option<tilo_resource::Shape> {
        Some(<Self as Model>::SHAPE)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the record's declared actions, each read top to bottom beside its parameters"
    )]
    fn declarations() -> Vec<ActionDecl<Self>> {
        let text = |name, description| ActionParam::required(name, ParamType::Text, description);
        let optional =
            |name, description| ActionParam::optional(name, ParamType::Text, description);
        vec![
            ActionDecl::new(
                "initialize-record",
                "Initialize the version marker without creating contacts or messages",
                Vec::new(),
                |_, now_ms| {
                    Ok(Stamped {
                        operation: MessagesOperation::InitializeRecord {},
                        now_ms,
                    })
                },
            ),
            ActionDecl::new(
                "deliver-message",
                "Deliver message",
                vec![
                    text("from", "The sender: a name or a phone number"),
                    optional("to", "The recipient, when known"),
                    text("body", "The message text"),
                    optional(
                        "date",
                        "When it was sent: RFC 3339 (2026-10-11T09:30:00Z); now when absent",
                    ),
                    optional(
                        "conversation",
                        "The conversation it belongs to; the sender's when absent",
                    ),
                ],
                |args, now_ms| {
                    build_delivery(args, now_ms).map(|delivery| Stamped {
                        operation: MessagesOperation::Deliver(delivery),
                        now_ms,
                    })
                },
            )
            .describe(
                "Adds a message to its conversation as if it had arrived, with no service or \
                 account: a seeded scenario, an import, an integration. Delivering the same \
                 message again changes nothing.",
            )
            .answering(),
            ActionDecl::new(
                "send-message",
                "Send message",
                vec![
                    text("to", "Who it is to: a name or a phone number"),
                    text("body", "The message text"),
                    optional(
                        "conversation",
                        "The conversation it belongs to; the recipient's when absent",
                    ),
                ],
                |args, now_ms| {
                    let to = args.text("to").unwrap_or_default().trim().to_owned();
                    let conversation = match args
                        .text("conversation")
                        .map(str::trim)
                        .filter(|c| !c.is_empty())
                    {
                        Some(_) => named_conversation(args)?,
                        None => conversation_key(&to)
                            .ok_or_else(|| format!("`{to}` names no one to write to"))?,
                    };
                    Ok(Stamped {
                        operation: MessagesOperation::Queue(Queued {
                            conversation,
                            with: Some(to),
                            body: args.text("body").unwrap_or_default().trim().to_owned(),
                        }),
                        now_ms,
                    })
                },
            )
            .describe(
                "Queues a message in the conversation with that person, starting it if there is \
                 none. Nothing is sent from here: a person sends it from a device holding the \
                 account.",
            )
            .answering(),
            ActionDecl::new(
                "reply",
                "Reply",
                vec![
                    text(
                        "conversation",
                        "The conversation answered: its id or messages.conversation:<id>",
                    ),
                    text("body", "The reply text"),
                ],
                |args, now_ms| {
                    Ok(Stamped {
                        operation: MessagesOperation::Queue(Queued {
                            conversation: named_conversation(args)?,
                            with: None,
                            body: args.text("body").unwrap_or_default().trim().to_owned(),
                        }),
                        now_ms,
                    })
                },
            )
            .describe(
                "Queues a reply in an existing conversation. Nothing is sent from here: a person \
                 sends it from a device holding the account.",
            )
            .answering(),
        ]
    }

    /// With no item, the conversations, most recent first; with
    /// `messages.conversation:<id>` one conversation whole, oldest first;
    /// with `messages.message:<id>` one message.
    const READABLE: bool = true;

    fn read_content(
        state: &Self,
        _doc: &Doc,
        item: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        read(state, item)
    }

    /// A delivery or a queued message answers with its conversation and
    /// message, or that it changed nothing.
    fn answer(
        _doc: &Doc,
        action: &str,
        changes: &[ui::automerge::Patch],
    ) -> Option<serde_json::Value> {
        if !matches!(action, "deliver-message" | "send-message" | "reply") {
            return None;
        }
        Some(made(changes).map_or_else(
            || serde_json::json!({ "unchanged": "the conversation already holds this message" }),
            |(conversation, message)| {
                let mut answer = serde_json::json!({
                    "conversation": format!("{CONVERSATION}{conversation}"),
                    "message": format!("{MESSAGE}{message}"),
                });
                if action != "deliver-message"
                    && let Some(fields) = answer.as_object_mut()
                {
                    fields.insert("status".into(), "queued".into());
                }
                answer
            },
        ))
    }
}

/// The conversation and message a change added: the entry it put into a
/// conversation's `messages`.
fn made(changes: &[ui::automerge::Patch]) -> Option<(String, String)> {
    use ui::automerge::{PatchAction, Prop};
    changes.iter().find_map(|patch| {
        let PatchAction::PutMap { key, .. } = &patch.action else {
            return None;
        };
        match patch.path.as_slice() {
            [
                (_, Prop::Map(root)),
                (_, Prop::Map(conversation)),
                (_, Prop::Map(_)),
                (_, Prop::Map(messages)),
            ] if root == "conversations" && messages == "messages" => {
                Some((conversation.clone(), key.clone()))
            }
            _ => None,
        }
    })
}

fn direction(message: &Message) -> &'static str {
    if message.outgoing { "out" } else { "in" }
}

fn snippet(body: &str) -> String {
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= SNIPPET_CHARS {
        flat
    } else {
        let mut cut: String = flat.chars().take(SNIPPET_CHARS).collect();
        cut.push('…');
        cut
    }
}

fn message_json(conversation: &Id, id: &Id, message: &Message) -> serde_json::Value {
    let mut value = serde_json::json!({
        "id": format!("{MESSAGE}{}", id.as_str()),
        "conversation": format!("{CONVERSATION}{}", conversation.as_str()),
        "from": message.from,
        "to": message.to,
        "body": message.body,
        "date": message.date,
        "direction": direction(message),
    });
    if message.queued
        && let Some(fields) = value.as_object_mut()
    {
        fields.insert("status".into(), "queued".into());
    }
    value
}

/// What `read` answers; see [`Messages::READABLE`].
fn read(state: &Messages, item: Option<&str>) -> Result<serde_json::Value, String> {
    let Some(item) = item.map(str::trim).filter(|item| !item.is_empty()) else {
        let mut listing: Vec<(i64, serde_json::Value)> = state
            .conversations
            .iter()
            .map(|(id, conversation)| {
                let last = conversation.messages.values().next_back();
                let queued = conversation
                    .messages
                    .values()
                    .filter(|message| message.queued)
                    .count();
                (
                    last.map_or(i64::MIN, |message| message.at_ms),
                    serde_json::json!({
                        "id": format!("{CONVERSATION}{}", id.as_str()),
                        "with": conversation.with,
                        "messages": conversation.messages.len(),
                        "queued": queued,
                        "last": last.map(|message| serde_json::json!({
                            "from": message.from,
                            "date": message.date,
                            "direction": direction(message),
                            "snippet": snippet(&message.body),
                        })),
                    }),
                )
            })
            .collect();
        listing.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
        return Ok(serde_json::json!({
            "conversations": listing.into_iter().map(|(_, value)| value).collect::<Vec<_>>(),
        }));
    };
    if let Some(message) = item.strip_prefix(MESSAGE) {
        let wanted = Id::from_raw(message);
        return state
            .conversations
            .iter()
            .find_map(|(key, conversation)| {
                conversation
                    .messages
                    .get(&wanted)
                    .map(|held| message_json(key, &wanted, held))
            })
            .ok_or_else(|| format!("no message `{item}`: read a conversation to list them"));
    }
    let key = conversation_key(item).map(Id::from_raw);
    let (key, conversation) = key
        .as_ref()
        .and_then(|key| state.conversations.get(key).map(|found| (key, found)))
        .ok_or_else(|| format!("no conversation `{item}`: read with no item to list them"))?;
    let skip = conversation.messages.len().saturating_sub(MOST_LISTED);
    Ok(serde_json::json!({
        "id": format!("{CONVERSATION}{}", key.as_str()),
        "with": conversation.with,
        "messages": conversation
            .messages
            .iter()
            .skip(skip)
            .map(|(id, message)| message_json(key, id, message))
            .collect::<Vec<_>>(),
    }))
}

/// The messages holding every word of `query` (any case) in their sender,
/// recipient, conversation or text, newest first.
fn search(state: &Messages, query: &str) -> Result<serde_json::Value, String> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Err("search for at least one word".into());
    }
    let mut found: Vec<(i64, serde_json::Value)> = state
        .conversations
        .iter()
        .flat_map(|(key, conversation)| {
            conversation
                .messages
                .iter()
                .filter(|(_, message)| {
                    let haystack = format!(
                        "{}\n{}\n{}\n{}",
                        conversation.with, message.from, message.to, message.body
                    )
                    .to_lowercase();
                    words.iter().all(|word| haystack.contains(word.as_str()))
                })
                .map(move |(id, message)| (message.at_ms, message_json(key, id, message)))
        })
        .collect();
    found.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    found.truncate(MOST_LISTED);
    Ok(serde_json::json!({
        "messages": found.into_iter().map(|(_, value)| value).collect::<Vec<_>>(),
    }))
}

/// The conversations `export-content` names, as `format`.
fn export(state: &Messages, format: &str, conversation: Option<&str>) -> Result<Vec<u8>, String> {
    let wanted = match conversation
        .map(str::trim)
        .filter(|named| !named.is_empty())
    {
        Some(named) => Some(
            conversation_key(named)
                .filter(|key| state.conversations.contains(&Id::from_raw(key.as_str())))
                .ok_or_else(|| format!("no conversation `{named}`"))?,
        ),
        None => None,
    };
    let chosen: Vec<(&Id, &Conversation)> = state
        .conversations
        .iter()
        .filter(|(id, _)| wanted.as_deref().is_none_or(|key| id.as_str() == key))
        .collect();
    match format {
        "json" => {
            let conversations: Vec<serde_json::Value> = chosen
                .iter()
                .map(|(key, conversation)| {
                    serde_json::json!({
                        "id": format!("{CONVERSATION}{}", key.as_str()),
                        "with": conversation.with,
                        "messages": conversation
                            .messages
                            .iter()
                            .map(|(id, message)| message_json(key, id, message))
                            .collect::<Vec<_>>(),
                    })
                })
                .collect();
            serde_json::to_vec_pretty(&serde_json::json!({ "conversations": conversations }))
                .map_err(|error| error.to_string())
        }
        "md" => {
            let mut out = String::from("# Messages\n");
            for (key, conversation) in chosen {
                let _ = write!(
                    out,
                    "\n## {} ({CONVERSATION}{})\n\n",
                    conversation.with,
                    key.as_str()
                );
                for message in conversation.messages.values() {
                    let who = if message.outgoing {
                        "You"
                    } else {
                        message.from.as_str()
                    };
                    let status = if message.queued { " (queued)" } else { "" };
                    let body = message.body.trim().replace('\n', "\n  ");
                    let _ = writeln!(out, "- {} **{who}**{status}: {body}", message.date);
                }
            }
            Ok(out.into_bytes())
        }
        other => Err(format!("no format `{other}`: md or json")),
    }
}

/// Answer the effects that read the record — searching it and exporting it —
/// from the record as it stands, the same in a person's window and an
/// agent's headless one. `None` for every other effect.
pub(crate) fn answer_effect(
    effect: &MessagesEffect,
    cx: &EffectCx<Messages>,
) -> Option<EffectAnswer> {
    match effect {
        MessagesEffect::SearchMessages { query } => {
            Some(search(&cx.live().get(), query).map(Effected::value))
        }
        MessagesEffect::ExportContent {
            format,
            conversation,
        } => Some(export_content(
            &cx.live().get(),
            format,
            conversation.as_deref(),
        )),
        _ => None,
    }
}

/// Keep the export in this window's content store, and answer with its
/// content address: the transcript stays in the room, never a file.
fn export_content(state: &Messages, format: &str, conversation: Option<&str>) -> EffectAnswer {
    let bytes = export(state, format, conversation)?;
    let content = ui::tilo_resource::content::current()
        .ok_or("this window keeps no content to export into")?;
    let hash = content
        .put(&bytes)
        .map_err(|error| format!("the export could not be kept: {error}"))?;
    Effected::content(&hash)
}

#[cfg(test)]
mod tests;
