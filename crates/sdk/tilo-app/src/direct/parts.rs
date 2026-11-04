//! A direct conversation's messages as a chain of parts.
//!
//! One record cannot hold a conversation that runs for months: a record's
//! snapshot has a size limit, and a newcomer would be sent all of it. So the
//! pair's record ([`super::DirectConversation`]) keeps who said
//! what to whom and how far each of them read, and its messages live in
//! numbered part records ([`DirectPart`]) of that same record
//! ([`RecordId::part`]), exactly as Chat keeps its channels' messages in
//! parts of the workspace's chat record.
//!
//! - **Which part is current.** The part with the highest number a device
//!   holds is the chain's head. A workspace's host replays a joining device the
//!   pair's record and the head only; every earlier part is on-demand
//!   ([`neighbour_wire::record_sync`]): a device opens it when someone reads
//!   back to it.
//! - **Where a message goes.** To the head, unless the head is full
//!   ([`post_part`]): then to the next part. A part's id is the pair's record
//!   and its number, so two writers that both start part `n + 1` start the
//!   same record, and their messages meet there.
//! - **Read marks** are positions in the chain ([`DirectReadMark`]), so they
//!   mean the same whether or not a device holds the earlier parts: a reader
//!   has read every message of the other person in the parts before `part`,
//!   and `seen` of them in `part`.
//! - **Counts of the parts before.** A part carries, for each part before it,
//!   how many messages each of the two people wrote there
//!   ([`DirectPart::earlier`]): whoever starts it writes them from the part
//!   before, which they hold. A reader's unread count reaches back to their
//!   mark through the head's counts, however many parts they were away for,
//!   without holding those parts.

use std::collections::BTreeMap;

use tilo_resource::{self, ActionCx, Collection, Id, JsonSchema, Model, Refusal};
use ui_core::record::RecordId;

use super::{DirectConversation, person};

/// The longest message text, in bytes: the door's own bound.
pub const MAX_TEXT_BYTES: usize = 16 * 1024;
/// Messages a part holds before the next one starts.
pub const PART_MESSAGES: usize = 2000;
/// Message text, in bytes, a part holds before the next one starts: well
/// under what one record's snapshot may hold, so every part stays
/// compactable whatever its messages say.
pub const PART_TEXT_BYTES: usize = 1024 * 1024;

/// Part `index` of the conversation kept in the pair's record `pair`.
#[cfg(test)]
#[must_use]
pub fn part(pair: &RecordId, index: u64) -> RecordId {
    pair.part(index)
}

/// The number of `record` when it is a part of the chain kept in `pair`.
#[cfg(test)]
#[must_use]
pub fn part_index(pair: &RecordId, record: &RecordId) -> Option<u64> {
    record
        .chain_part()
        .filter(|(base, _)| base == pair)
        .map(|(_, index)| index)
}

/// The part a new message goes to, given the head (the highest part held),
/// how many messages it holds and their text in bytes: the head, or the next
/// part once the head is full.
#[must_use]
pub const fn post_part(head: u64, head_messages: usize, head_text_bytes: usize) -> u64 {
    if head_messages >= PART_MESSAGES || head_text_bytes >= PART_TEXT_BYTES {
        head.saturating_add(1)
    } else {
        head
    }
}

/// A message of a direct conversation.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct DirectMessage {
    /// The person who wrote it, in the key a conversation keeps people
    /// under.
    #[model(label, readonly)]
    pub author: String,
    /// What it says.
    #[model(readonly)]
    pub text: String,
    /// When its writer's device sent it, in milliseconds since the epoch.
    #[model(readonly)]
    pub at: i64,
}

/// A message as the old private store keeps it, for the import.
#[derive(
    Model, Default, Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, JsonSchema,
)]
pub struct StoredMessage {
    pub id: String,
    pub author: String,
    pub text: String,
    #[serde(default)]
    pub at: i64,
}

/// One part of a direct conversation's messages: every message sent while it
/// was the head, oldest first.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct DirectPart {
    /// The messages, oldest first: made, ordered and removed only by the
    /// part's actions. A message takes its author from whoever signed it; a
    /// generic insert could name anyone as a message's author, and a replica
    /// replaying it would reproduce the forgery.
    #[model(readonly)]
    pub messages: Collection<DirectMessage>,
    /// For each part before this one (by its number, in decimal), how many
    /// messages each person (by their conversation key) wrote there, as
    /// whoever started this part saw it
    /// ([`DirectPart::carry_counts`]). A message that reached a part after
    /// the next one started is counted where that part is held: its own
    /// messages count, not this.
    #[model(readonly)]
    pub earlier: BTreeMap<String, BTreeMap<String, i64>>,
}

impl DirectPart {
    /// The message text of the part, in bytes: what fills it
    /// ([`PART_TEXT_BYTES`]).
    #[must_use]
    pub fn text_bytes(&self) -> usize {
        self.messages
            .values()
            .map(|message| message.text.len())
            .sum()
    }

    /// How many messages each person wrote in this part, by their
    /// conversation key.
    #[must_use]
    pub fn counts(&self) -> BTreeMap<String, i64> {
        let mut counts: BTreeMap<String, i64> = BTreeMap::new();
        for message in self.messages.values() {
            let count = counts.entry(message.author.clone()).or_default();
            *count = count.saturating_add(1);
        }
        counts
    }

    /// What a part started after this one, part `index`, carries of the
    /// parts before it: what this one carries, and this one's own counts.
    #[must_use]
    pub fn carried_after(&self, index: u64) -> BTreeMap<String, BTreeMap<String, i64>> {
        let mut earlier = self.earlier.clone();
        earlier.insert(index.to_string(), self.counts());
        earlier
    }
}

/// How far a person read the conversation: every message of the other person
/// in the parts before `part`, and the first `seen` of them in `part`.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DirectReadMark {
    /// The part the person read into.
    pub part: i64,
    /// Messages of the other person they saw in that part.
    pub seen: i64,
}

/// The message id an invocation sends: the same invocation is the same
/// message, so a retry stores nothing twice.
#[must_use]
pub fn message_id(invocation: tilo_actions::InvocationId) -> String {
    format!("dm_{}", invocation.0)
}

fn stored_key(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_message_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn valid_text(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= MAX_TEXT_BYTES
}

#[tilo_resource::resource(
    kind = "chat.direct-messages",
    label = "Direct conversation messages",
    params = direct_part,
)]
impl DirectPart {
    /// Say something.
    ///
    /// Stores the message under `id` once: the same id is the same message,
    /// so a send that is asked for twice says it once.
    #[action]
    pub fn send(
        &mut self,
        cx: &mut ActionCx,
        #[param("The message's id")] id: String,
        #[param("What it says")] text: String,
    ) -> Result<(), Refusal> {
        if !valid_message_id(&id) {
            return Err(Refusal::new("the message's id is not a message id"));
        }
        if !valid_text(&text) {
            return Err(Refusal::new("the message is empty or too long"));
        }
        let author = person(cx)?;
        let id = Id::from_raw(id);
        if self.messages.contains(&id) {
            return Ok(());
        }
        self.messages.push(
            id,
            DirectMessage {
                author,
                text,
                at: cx.now_ms(),
            },
        );
        Ok(())
    }

    /// Take in messages kept in the old private store.
    ///
    /// Stores each message under the id it was kept with: importing the same
    /// conversation again adds nothing.
    #[action(housekeeping)]
    pub fn import(
        &mut self,
        _cx: &mut ActionCx,
        #[param("The stored messages")] messages: Vec<StoredMessage>,
    ) -> Result<(), Refusal> {
        for message in messages {
            if !valid_message_id(&message.id) {
                return Err(Refusal::new("a stored message has no valid id"));
            }
            if !stored_key(&message.author) {
                return Err(Refusal::new("a stored message has no author"));
            }
            if !valid_text(&message.text) {
                return Err(Refusal::new("a stored message is empty or too long"));
            }
            let id = Id::from_raw(message.id);
            if self.messages.contains(&id) {
                continue;
            }
            self.messages.push(
                id,
                DirectMessage {
                    author: message.author,
                    text: message.text,
                    at: message.at,
                },
            );
        }
        Ok(())
    }

    /// Write down how many messages the parts before this one hold, for each
    /// person: what a part started from the one before carries
    /// ([`DirectPart::earlier`]).
    #[action(housekeeping)]
    pub fn carry_counts(
        &mut self,
        _cx: &mut ActionCx,
        #[param("Each person's messages, by part")] earlier: BTreeMap<
            String,
            BTreeMap<String, i64>,
        >,
    ) {
        self.earlier = earlier;
    }
}

impl crate::framework::Chained for DirectPart {
    type Base = DirectConversation;

    /// Unused: every direct conversation has a chain of its own, beside the
    /// pair's record ([`Self::chain_beside`]), never the app's one chain.
    fn chain() -> RecordId {
        RecordId::derived("direct-messages", &RecordId::workspace_default())
    }

    /// The parts of the pair's record `base`: the conversation kept in it.
    fn chain_beside(base: &RecordId) -> RecordId {
        base.clone()
    }

    fn part_for(action: &Self::Action, parts: &[(u64, std::rc::Rc<Self>)]) -> Result<u64, String> {
        let new = || {
            Ok(parts.last().map_or(0, |(head, part)| {
                post_part(*head, part.messages.len(), part.text_bytes())
            }))
        };
        match &action.operation {
            // A new message, an import and the counts a part starts with
            // all go to the head, or the part after it once it is full. The
            // counts are carried by the framework on the part it starts,
            // never asked for on its own.
            DirectPartAction::Send(_)
            | DirectPartAction::Import(_)
            | DirectPartAction::CarryCounts(_) => new(),
            // A field edit of the part (a message inserted whole, as a
            // scenario stages one) goes to the head.
            DirectPartAction::FieldEdit(_) => Ok(parts.last().map_or(0, |(head, _)| *head)),
        }
    }

    fn carry(previous: u64, part: &Self) -> Option<tilo_resource::Invoke<Self>> {
        Some(Self::CARRY_COUNTS.with(direct_part::CarryCounts {
            earlier: part.carried_after(previous),
        }))
    }
}

/// The messages of a conversation this window holds, oldest part first: the
/// timeline a conversation is read and counted from.
pub struct Conversation {
    parts: Vec<(u64, std::rc::Rc<DirectPart>)>,
}

impl Conversation {
    #[must_use]
    pub fn new(mut parts: Vec<(u64, std::rc::Rc<DirectPart>)>) -> Self {
        parts.sort_by_key(|(index, _)| *index);
        Self { parts }
    }

    /// The parts held, oldest first.
    #[must_use]
    pub fn parts(&self) -> &[(u64, std::rc::Rc<DirectPart>)] {
        &self.parts
    }

    /// Every message held, oldest part first and, in a part, in the order it
    /// was sent.
    pub fn messages(&self) -> impl Iterator<Item = (&Id, &DirectMessage)> {
        self.parts.iter().flat_map(|(_, part)| part.messages.iter())
    }

    /// Whether nothing was said here yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.parts.iter().all(|(_, part)| part.messages.is_empty())
    }

    /// The part holding the message `id`.
    #[must_use]
    pub fn part_of(&self, id: &Id) -> Option<u64> {
        self.parts
            .iter()
            .rev()
            .find(|(_, part)| part.messages.contains(id))
            .map(|(index, _)| *index)
    }

    /// The part a new message goes to ([`post_part`]).
    #[must_use]
    pub fn post_part(&self) -> u64 {
        self.parts.last().map_or(0, |(head, part)| {
            post_part(*head, part.messages.len(), part.text_bytes())
        })
    }

    /// How far someone who read every message of `peer` held has read: the
    /// newest part with one of them, and how many it has.
    #[must_use]
    pub fn read_to_end(&self, peer: &str) -> DirectReadMark {
        self.parts
            .iter()
            .rev()
            .find_map(|(index, part)| {
                let seen = part
                    .messages
                    .values()
                    .filter(|message| message.author == peer)
                    .count();
                (seen > 0).then(|| DirectReadMark {
                    part: i64::try_from(*index).unwrap_or(i64::MAX),
                    seen: i64::try_from(seen).unwrap_or(i64::MAX),
                })
            })
            .unwrap_or_default()
    }

    /// The messages of `peer` after `mark`: counted in each part held, and in
    /// the parts between that are not, as the head carries them
    /// ([`DirectPart::earlier`]).
    #[must_use]
    pub fn unread(&self, peer: &str, mark: DirectReadMark) -> u32 {
        let Some((head, newest)) = self.parts.last() else {
            return 0;
        };
        let from = u64::try_from(mark.part.max(0)).unwrap_or(u64::MAX);
        let seen = mark.seen.max(0);
        let part_held = |index: u64| {
            self.parts
                .binary_search_by_key(&index, |(held, _)| *held)
                .ok()
                .and_then(|at| self.parts.get(at))
                .map(|(_, part)| part)
        };
        let count: i64 = (from..=*head)
            .map(|index| {
                let total = match part_held(index) {
                    Some(part) => part
                        .messages
                        .values()
                        .filter(|message| message.author == peer)
                        .count()
                        .try_into()
                        .unwrap_or(i64::MAX),
                    None => newest
                        .earlier
                        .get(&index.to_string())
                        .and_then(|counts| counts.get(peer))
                        .copied()
                        .unwrap_or_default(),
                };
                if index == from {
                    total.saturating_sub(seen)
                } else {
                    total
                }
            })
            .sum();
        u32::try_from(count.max(0)).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests;
