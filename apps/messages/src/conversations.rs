//! The Messages record: conversations put into it on purpose, and the
//! messages delivered into them or written in them, held whole.
//!
//! ## Why message text is in the record
//!
//! A person's own service histories (SMS, Signal, `WhatsApp`, Tilo) never
//! enter it: those stay the privileged broker's account-owned data, read
//! through `crate::history` on a device holding the account. What this
//! record holds is what was put into it on purpose: messages *delivered*
//! into it (a seeded scenario, an import, an integration relaying a text)
//! and messages *written* in it, queued for a person to send. The person
//! approved giving agents access to that content. An agent works on a
//! room's records only through their published operations, in a confined
//! worker with no network and no account, so a conversation it is to read
//! or answer has to be in the record it holds, as Mail's delivered mail is.
//! Whoever the record is shared with reads these messages, so nothing
//! reaches it except through `deliver-message`, `send-message` and `reply`
//! (the collection is read-only to field edits).
//!
//! ## Nothing is sent from here
//!
//! `send-message` and `reply` only *queue* a message in its conversation:
//! no transport is reached from a record operation, least of all from an
//! agent's worker. A person sends it from a device holding the account.
//!
//! ## Deterministic addresses
//!
//! A conversation's id comes from what the caller names (the `conversation`
//! given, else the person it is with), normalised so `+1 555 0100` and
//! `+15550100` are one conversation and `Ada Lovelace` is `ada-lovelace`. A
//! delivered message's id comes from what it says. So a written step can
//! name `tilo.messages/conversations/<id>` before it runs, and delivering the
//! same message twice changes nothing.

use std::fmt::Write as _;

use ui::prelude::{Collection, Id, Model};
use ui::tilo_resource::items::{item_of_resource, item_resource_id};
use ui::tilo_resource::{self, ActionCx, Refusal, resource};

/// Bounds on what one message carries, so a hostile payload cannot grow the
/// record without limit.
const MAX_PARTY_CHARS: usize = 256;
const MAX_BODY_CHARS: usize = 16 * 1024;
const MAX_KEY_CHARS: usize = 64;

/// How much of a body a listing shows.
const SNIPPET_CHARS: usize = 160;
/// The most messages one read or search answers with, so an answer fits
/// what an agent is shown of it.
const MOST_LISTED: usize = 200;

/// The start of a conversation's item address
/// (`tilo.messages/conversations/<id>`), which a caller may name it by.
const CONVERSATION_PREFIX: &str = "tilo.messages/conversations/";

/// The record. Its conversations are keyed by their deterministic ids.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Messages {
    /// The conversations, keyed by [`conversation_key`]: made and changed
    /// only by the record's verbs, so no field edit can put words in
    /// someone's mouth or mark a message sent.
    #[model(readonly)]
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

#[resource(
    kind = "tilo.messages",
    label = "Messages",
    read_content = Messages::read,
    answer = answer,
)]
impl Messages {
    /// Deliver message.
    ///
    /// Adds a message to its conversation as if it had arrived, with no
    /// service or account: a seeded scenario, an import, an integration.
    /// Delivering the same message again changes nothing.
    #[action(external, answers, audience = "agents")]
    pub fn deliver_message(
        &mut self,
        cx: &mut ActionCx,
        #[param("The sender: a name or a phone number")] from: String,
        #[param("The recipient, when known")] to: Option<String>,
        #[param("The message text")] body: String,
        #[param("When it was sent, RFC 3339 (2026-10-11T09:30:00Z); now when absent")] date: Option<
            String,
        >,
        #[param("The conversation it belongs to; the sender's when absent")] conversation: Option<
            String,
        >,
    ) -> Result<(), Refusal> {
        let from = from.trim().to_owned();
        if from.is_empty() {
            return Err(Refusal::new("`from` names the sender: a name or a number"));
        }
        let to = to.map(|to| to.trim().to_owned()).unwrap_or_default();
        let body = body.trim().to_owned();
        if body.is_empty() {
            return Err(Refusal::new("a message needs words"));
        }
        check_message(&from, &to, &body)?;
        let at_ms = match date.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            Some(date) => parse_date(date).ok_or_else(|| {
                Refusal::new(format!(
                    "`{date}` is not a date: write 2026-10-11T09:30:00Z"
                ))
            })?,
            None => cx.now_ms(),
        };
        let date = format_date(at_ms);
        let named = conversation
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty());
        let key = match named {
            Some(named) => conversation_key(named)
                .ok_or_else(|| Refusal::new(format!("`{named}` is not a conversation id")))?,
            None => conversation_key(&from).ok_or_else(|| {
                Refusal::new(format!("`{from}` names no one a conversation can be with"))
            })?,
        };
        let id = Id::from_raw(derived_id(&[&key, &from, &to, &date, &body]));
        let key = Id::from_raw(key);
        if !self.conversations.contains(&key) {
            self.conversations.push(
                key.clone(),
                Conversation {
                    with: from.clone(),
                    messages: Collection::new(),
                },
            );
        }
        let held = self
            .conversations
            .get_mut(&key)
            .ok_or("the conversation could not be made")?;
        // A message delivered before is left as it was.
        if held.messages.contains(&id) {
            return Ok(());
        }
        insert_in_order(
            &mut held.messages,
            id,
            Message {
                from,
                to,
                body,
                date,
                at_ms,
                outgoing: false,
                queued: false,
            },
        );
        Ok(())
    }

    /// Send message.
    ///
    /// Queues a message in the conversation with that person, starting it
    /// if there is none. Nothing is sent from here: a person sends it from
    /// a device holding the account.
    #[action(answers, audience = "agents")]
    pub fn send_message(
        &mut self,
        cx: &mut ActionCx,
        #[param("Who it is to: a name or a phone number")] to: String,
        #[param("The message text")] body: String,
        #[param("The conversation it belongs to; the recipient's when absent")]
        conversation: Option<String>,
    ) -> Result<(), Refusal> {
        let to = to.trim().to_owned();
        let named = conversation
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty());
        let key = match named {
            Some(named) => conversation_key(named),
            None => conversation_key(&to),
        }
        .ok_or_else(|| Refusal::new(format!("`{to}` names no one to write to")))?;
        self.queue(cx, &Id::from_raw(key), Some(to), &body)
    }

    /// Reply.
    ///
    /// Queues a reply in an existing conversation. Nothing is sent from
    /// here: a person sends it from a device holding the account.
    #[action(item = conversations, answers, audience = "agents")]
    pub fn reply(
        &mut self,
        cx: &mut ActionCx,
        conversation: &Id,
        #[param("The reply text")] body: String,
    ) -> Result<(), Refusal> {
        let key = conversation_key(conversation.as_str())
            .ok_or_else(|| Refusal::new(format!("`{conversation}` is not a conversation id")))?;
        self.queue(cx, &Id::from_raw(key), None, &body)
    }
}

impl Messages {
    /// Queue `body` in conversation `key`, starting it with `with` when it
    /// is new.
    fn queue(
        &mut self,
        cx: &mut ActionCx,
        key: &Id,
        with: Option<String>,
        body: &str,
    ) -> Result<(), Refusal> {
        let body = body.trim();
        if body.is_empty() {
            return Err(Refusal::new("a message needs words"));
        }
        check_message(with.as_deref().unwrap_or_default(), "", body)?;
        if !self.conversations.contains(key) {
            let with = with.filter(|with| !with.is_empty()).ok_or_else(|| {
                Refusal::new(format!(
                    "no conversation `{key}`: `read` lists them, or `send-message` starts one"
                ))
            })?;
            self.conversations.push(
                key.clone(),
                Conversation {
                    with,
                    messages: Collection::new(),
                },
            );
        }
        let held = self
            .conversations
            .get_mut(key)
            .ok_or("the conversation could not be made")?;
        let at_ms = cx.now_ms();
        let message = Message {
            from: String::new(),
            to: held.with.clone(),
            body: body.to_owned(),
            date: format_date(at_ms),
            at_ms,
            outgoing: true,
            queued: true,
        };
        insert_in_order(&mut held.messages, cx.id(), message);
        Ok(())
    }

    /// What `read` answers: with no item, the conversations, most recent
    /// first; with a conversation (its id, its item address or who it is
    /// with), its latest messages, oldest first; with a message's id, that
    /// message.
    ///
    /// # Errors
    /// When `item` names no conversation or message here.
    pub fn read(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
        let Some(item) = item
            .map(Id::as_str)
            .map(str::trim)
            .filter(|i| !i.is_empty())
        else {
            return Ok(self.listing());
        };
        let bare = item_of_resource::<Self>(item)
            .map_or_else(|| item.to_owned(), |(_, id)| id.as_str().to_owned());
        if let Some(found) = self.message(&bare) {
            return Ok(found);
        }
        let (key, conversation) = self
            .conversation(&bare)
            .ok_or_else(|| format!("no conversation `{item}`: read with no item to list them"))?;
        let skip = conversation.messages.len().saturating_sub(MOST_LISTED);
        Ok(serde_json::json!({
            "id": conversation_address(key),
            "with": conversation.with,
            "messages": conversation
                .messages
                .iter()
                .skip(skip)
                .map(|(id, message)| message_json(key, id, message))
                .collect::<Vec<_>>(),
        }))
    }

    /// Every conversation, the one with the latest message first.
    fn listing(&self) -> serde_json::Value {
        let mut listing: Vec<(i64, serde_json::Value)> = self
            .conversations
            .iter()
            .map(|(id, conversation)| {
                let last = conversation.messages.values().next_back();
                let queued = conversation.messages.values().filter(|m| m.queued).count();
                (
                    last.map_or(i64::MIN, |message| message.at_ms),
                    serde_json::json!({
                        "id": conversation_address(id),
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
        serde_json::json!({
            "conversations": listing.into_iter().map(|(_, value)| value).collect::<Vec<_>>(),
        })
    }

    /// The message `id`, in whichever conversation holds it.
    fn message(&self, id: &str) -> Option<serde_json::Value> {
        let wanted = Id::from_raw(id);
        self.conversations.iter().find_map(|(key, conversation)| {
            conversation
                .messages
                .get(&wanted)
                .map(|held| message_json(key, &wanted, held))
        })
    }

    /// The conversation `named` names: by its id, or who it is with.
    pub fn conversation(&self, named: &str) -> Option<(&Id, &Conversation)> {
        let key = Id::from_raw(conversation_key(named)?);
        self.conversations.iter().find(|(held, _)| **held == key)
    }

    /// The messages holding every word of `query` (any case) in their
    /// sender, recipient, conversation or text, newest first.
    ///
    /// # Errors
    /// When the query has no words.
    pub fn search(&self, query: &str) -> Result<serde_json::Value, String> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return Err("search for some words".into());
        }
        let mut hits: Vec<(i64, serde_json::Value)> = self
            .conversations
            .iter()
            .flat_map(|(key, conversation)| {
                let words = &words;
                conversation
                    .messages
                    .iter()
                    .filter_map(move |(id, message)| {
                        let haystack = format!(
                            "{} {} {} {} {}",
                            conversation.with, key, message.from, message.to, message.body
                        )
                        .to_lowercase();
                        words
                            .iter()
                            .all(|word| haystack.contains(word.as_str()))
                            .then(|| (message.at_ms, message_json(key, id, message)))
                    })
            })
            .collect();
        hits.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
        let total = hits.len();
        hits.truncate(MOST_LISTED);
        Ok(serde_json::json!({
            "query": query.trim(),
            "total": total,
            "messages": hits.into_iter().map(|(_, value)| value).collect::<Vec<_>>(),
        }))
    }

    /// The transcript of one conversation, or of every one, as `format`
    /// (`md`, the default, or `json`): what `export-content` keeps as
    /// content. Answers the bytes and how many messages they hold.
    ///
    /// # Errors
    /// An unknown format, or a conversation that is not here.
    pub fn transcript(
        &self,
        format: Option<&str>,
        conversation: Option<&str>,
    ) -> Result<(Vec<u8>, usize), String> {
        let chosen: Vec<(&Id, &Conversation)> =
            match conversation.map(str::trim).filter(|c| !c.is_empty()) {
                Some(named) => {
                    let bare = item_of_resource::<Self>(named)
                        .map_or_else(|| named.to_owned(), |(_, id)| id.as_str().to_owned());
                    vec![self.conversation(&bare).ok_or_else(|| {
                        format!("no conversation `{named}`: read with no item to list them")
                    })?]
                }
                None => self.conversations.iter().collect(),
            };
        let count = chosen.iter().map(|(_, c)| c.messages.len()).sum();
        let bytes = match format.map_or("md", str::trim) {
            "md" | "markdown" | "" => markdown(&chosen).into_bytes(),
            "json" => serde_json::to_vec_pretty(&serde_json::json!({
                "conversations": chosen
                    .iter()
                    .map(|(key, conversation)| serde_json::json!({
                        "id": conversation_address(key),
                        "with": conversation.with,
                        "messages": conversation
                            .messages
                            .iter()
                            .map(|(id, message)| message_json(key, id, message))
                            .collect::<Vec<_>>(),
                    }))
                    .collect::<Vec<_>>(),
            }))
            .map_err(|error| error.to_string())?,
            other => return Err(format!("`{other}` is not a format: md or json")),
        };
        Ok((bytes, count))
    }
}

/// What `export-content` answers once the transcript is kept as `hash`:
/// its content address (also as the operation's resource, so the room keeps
/// it), the format and how much it holds.
#[must_use]
pub fn exported(
    hash: &str,
    format: Option<&str>,
    messages: usize,
    bytes: usize,
) -> ui::framework::Effected {
    let address = ui::tilo_actions::content_resource(hash);
    ui::framework::Effected::Done(ui::tilo_actions::ActionOutput {
        value: Some(serde_json::json!({
            "content": address,
            "format": match format.map(str::trim) {
                Some("json") => "json",
                _ => "md",
            },
            "messages": messages,
            "bytes": bytes,
        })),
        resources: address.into_iter().collect(),
    })
}

/// A transcript in Markdown: a heading per conversation, a line per message.
fn markdown(chosen: &[(&Id, &Conversation)]) -> String {
    let mut out = String::new();
    for (key, conversation) in chosen {
        let _ = writeln!(out, "# {}\n", heading(conversation, key));
        for message in conversation.messages.values() {
            let who = if message.outgoing {
                "You"
            } else {
                message.from.as_str()
            };
            let status = if message.queued { " (queued)" } else { "" };
            let _ = writeln!(
                out,
                "- **{who}** ({}){status}: {}",
                message.date,
                message.body.replace('\n', "\n  ")
            );
        }
        out.push('\n');
    }
    out
}

fn heading(conversation: &Conversation, key: &Id) -> String {
    if conversation.with.is_empty() {
        key.as_str().to_owned()
    } else {
        conversation.with.clone()
    }
}

/// The address a step names a conversation by.
fn conversation_address(key: &Id) -> String {
    item_resource_id::<Messages>("conversations", key)
}

/// The conversation and message a change added: the entry it put into a
/// conversation's messages.
fn made(changes: &[ui::automerge::Patch]) -> Option<(String, String)> {
    use ui::automerge::{PatchAction, Prop};
    changes.iter().find_map(|patch| {
        let PatchAction::PutMap { key, .. } = &patch.action else {
            return None;
        };
        let props: Vec<&str> = patch
            .path
            .iter()
            .filter_map(|(_, prop)| match prop {
                Prop::Map(name) => Some(name.as_str()),
                Prop::Seq(_) => None,
            })
            .collect();
        match props.as_slice() {
            ["conversations", conversation, .., "messages"] => {
                Some(((*conversation).to_owned(), key.clone()))
            }
            _ => None,
        }
    })
}

/// A delivery or a queued message answers with its conversation and
/// message, or that it changed nothing.
fn answer(
    _doc: &tilo_resource::Doc,
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
                "conversation": conversation_address(&Id::from_raw(conversation)),
                "message": message,
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
        return Err(Refusal::new(format!(
            "a sender or recipient is at most {MAX_PARTY_CHARS} characters"
        )));
    }
    if body.chars().count() > MAX_BODY_CHARS {
        return Err(Refusal::new(format!(
            "a message is at most {MAX_BODY_CHARS} characters"
        )));
    }
    Ok(())
}

/// The id of the conversation with `party`, or the one a caller named: a
/// number keeps its digits (and a leading `+`), anything else becomes a
/// lowercase slug. `None` when nothing of it is left.
#[must_use]
pub fn conversation_key(party: &str) -> Option<String> {
    let party = party.trim();
    let party = party
        .strip_prefix(CONVERSATION_PREFIX)
        .unwrap_or(party)
        .trim();
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
        slug
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

/// `text` as an instant in milliseconds: `YYYY-MM-DD`, optionally with a
/// time `THH:MM[:SS[.fff]]` and a zone (`Z` or `±HH:MM`; UTC when absent).
/// Dates come from callers, so nothing here can overflow: an impossible one
/// reads as none.
pub(crate) fn parse_date(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, time) = match text.split_once(['T', 't', ' ']) {
        Some((date, time)) => (date, time),
        None => (text, ""),
    };
    let mut fields = date.split('-');
    let year: i64 = fields.next()?.parse().ok()?;
    let month: u32 = fields.next()?.parse().ok()?;
    let day: u32 = fields.next()?.parse().ok()?;
    if fields.next().is_some()
        || !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || day == 0
        || day > ui::date::days_in_month(year, month)
    {
        return None;
    }
    let (clock, offset_minutes) = split_zone(time)?;
    let (seconds_of_day, millis) = if clock.is_empty() {
        (0, 0)
    } else {
        clock_of(clock)?
    };
    let days = ui::date::days_from_civil(year, month, day);
    days.checked_mul(86_400)?
        .checked_add(seconds_of_day)?
        .checked_sub(offset_minutes.checked_mul(60)?)?
        .checked_mul(1000)?
        .checked_add(millis)
}

/// `HH:MM[:SS[.fff]]` as seconds into the day and milliseconds.
fn clock_of(clock: &str) -> Option<(i64, i64)> {
    let (whole, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut parts = whole.split(':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let second: i64 = parts.next().map_or(Some(0), |s| s.parse().ok())?;
    if parts.next().is_some() || !(0..=23).contains(&hour) || !(0..=59).contains(&minute) {
        return None;
    }
    if !(0..=60).contains(&second) {
        return None;
    }
    let seconds = hour
        .checked_mul(3600)?
        .checked_add(minute.checked_mul(60)?)?
        .checked_add(second)?;
    // Milliseconds from the first three digits of the fraction, padded.
    let mut millis = 0_i64;
    let mut digits = fraction.chars();
    for _ in 0..3 {
        let digit = digits.next().map_or(Some(0), |ch| ch.to_digit(10))?;
        millis = millis.checked_mul(10)?.checked_add(i64::from(digit))?;
    }
    Some((seconds, millis))
}

/// A clock and its zone: `Z`, `±HH:MM` or nothing (UTC), as minutes east.
fn split_zone(time: &str) -> Option<(&str, i64)> {
    if let Some(clock) = time.strip_suffix(['Z', 'z']) {
        return Some((clock, 0));
    }
    match time.rfind(['+', '-']) {
        Some(at) => {
            let (clock, zone) = time.split_at(at);
            let (hours, minutes) = zone.get(1..)?.split_once(':')?;
            let hours: i64 = hours.parse().ok()?;
            let minutes: i64 = minutes.parse().ok()?;
            if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
                return None;
            }
            let east = hours.checked_mul(60)?.checked_add(minutes)?;
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            Some((clock, east.checked_mul(sign)?))
        }
        None => Some((time, 0)),
    }
}

/// `at_ms` as RFC 3339 in UTC, to the second.
pub(crate) fn format_date(at_ms: i64) -> String {
    let seconds = at_ms.div_euclid(1000);
    let (year, month, day) = ui::date::civil_from_days(seconds.div_euclid(86_400));
    let of_day = seconds.rem_euclid(86_400);
    // format: machine (RFC 3339 in UTC, as stored and exchanged, never shown)
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60
    )
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
        "id": id.as_str(),
        "conversation": conversation_address(conversation),
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

#[cfg(test)]
mod tests;
