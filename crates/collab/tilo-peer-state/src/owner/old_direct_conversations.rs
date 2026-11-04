//! Two-person text an older build kept in the workspace's owner file, read
//! once so the conversation's own record can take it over, and then emptied.
//!
//! A conversation between people is an ordinary record of the workspace now
//! (`direct/<pair>`, kept by the app whose stores hold conversations), and
//! nothing writes this store: a device that still holds it reads its messages
//! as the import's pages and empties it in one publication. The stored shapes
//! below are the ones those builds wrote, byte for byte, so their files still
//! read.
//!
//! This module goes in the release after the first one that ships the import:
//! by then every device has had a release in which the conversations it held
//! moved into their records.

use crate::caller_authority::CallerAuthority;
use crate::owner::{NativeCommittedView, NativeWorkspaceOwner, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use web_time::SystemTime;

const MAX_CONVERSATIONS: usize = 1024;
const MAX_MESSAGES: usize = 4096;
const MAX_TEXT_BYTES: usize = 16_384;
const MAX_PAGE: usize = 100;

/// What the owner answers when the store holds no conversation with a peer.
pub const OLD_CONVERSATION_NOT_OPENED: &str = "private conversation not opened";

/// One message as the old store kept it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OldMessage {
    pub id: String,
    /// The sender under the key the store names people by.
    pub sender: String,
    pub sequence: u64,
    /// Unix seconds assigned by the owner, never supplied by the sender.
    pub received_at: i64,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OldConversationPage {
    pub after_sequence: u64,
    pub limit: usize,
}
impl Default for OldConversationPage {
    fn default() -> Self {
        Self {
            after_sequence: 0,
            limit: 50,
        }
    }
}

/// One page of a conversation as the old store holds it, read to import it
/// into the conversation's record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OldConversation {
    pub id: String,
    pub peer: String,
    pub messages: Vec<OldMessage>,
    pub next_sequence: u64,
    pub has_more: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pair {
    /// Files written before the engine's vocabulary named the workspace carry
    /// `room`; the key stays so they read back.
    #[serde(rename = "room")]
    workspace: WorkspaceId,
    people: [String; 2],
    messages: Vec<OldMessage>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Invocation {
    conversation: String,
    source: [u8; 32],
    body_hash: [u8; 32],
    message_id: String,
}

/// The stored conversations, as an owner file written before the import
/// carries them. Never written: read to import and then emptied.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OldDirectConversations {
    version: u16,
    pairs: BTreeMap<String, Pair>,
    /// Message receipts, kept so an older file's bytes read back whole. The
    /// import never asks for them.
    #[serde(default)]
    invocations: BTreeMap<uuid::Uuid, Invocation>,
}
impl Default for OldDirectConversations {
    fn default() -> Self {
        Self {
            version: 1,
            pairs: BTreeMap::new(),
            invocations: BTreeMap::new(),
        }
    }
}

fn key(bytes: [u8; 32]) -> String {
    blake3::Hash::from_bytes(bytes).to_hex().to_string()
}

/// How the store names a person from its root public key. The same hash names
/// the conversations' pairs, so a caller holding the root key can find the
/// person's conversations.
pub fn person_store_key(bytes: [u8; 32]) -> String {
    key(bytes)
}

fn validate_key(value: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("person key must be canonical lowercase public-key hex".into());
    }
    Ok(())
}
fn text_valid(text: &str) -> Result<(), String> {
    if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES {
        return Err("private message is empty or exceeds UTF-8 byte bound".into());
    }
    Ok(())
}

impl OldDirectConversations {
    /// Refuse a stored store that no store ever wrote: bounded, canonical and
    /// in sequence, so a corrupt file imports nothing rather than nonsense.
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || self.pairs.len() > MAX_CONVERSATIONS
            || self.invocations.len() > MAX_MESSAGES
        {
            return Err("invalid private conversation version or capacity".into());
        }
        let mut count = 0usize;
        for pair in self.pairs.values() {
            for person in &pair.people {
                validate_key(person)?;
            }
            count = count
                .checked_add(pair.messages.len())
                .ok_or("private message count overflow")?;
            if count > MAX_MESSAGES {
                return Err("private conversation message capacity exhausted".into());
            }
            for (index, message) in pair.messages.iter().enumerate() {
                text_valid(&message.text)?;
                if message.sequence
                    != (index as u64)
                        .checked_add(1)
                        .ok_or("private sequence overflow")?
                    || !pair.people.contains(&message.sender)
                    || message.received_at < 0
                {
                    return Err("invalid private conversation message".into());
                }
            }
        }
        Ok(())
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.pairs.is_empty() && self.invocations.is_empty()
    }
    /// One page of the conversation between `me` and `peer`.
    fn conversation(
        &self,
        me: &str,
        peer: &str,
        page: OldConversationPage,
    ) -> Result<Option<OldConversation>, String> {
        if page.limit == 0 || page.limit > MAX_PAGE {
            return Err("private conversation page limit out of bounds".into());
        }
        let Some((id, pair)) = self.pairs.iter().find(|(_, pair)| {
            pair.people.contains(&me.to_owned()) && pair.people.contains(&peer.to_owned())
        }) else {
            return Ok(None);
        };
        if page.after_sequence > pair.messages.len() as u64 {
            return Err("private conversation cursor out of bounds".into());
        }
        let messages: Vec<_> = pair
            .messages
            .iter()
            .filter(|m| m.sequence > page.after_sequence)
            .take(page.limit)
            .cloned()
            .collect();
        let next_sequence = messages.last().map_or(page.after_sequence, |m| m.sequence);
        Ok(Some(OldConversation {
            id: id.clone(),
            peer: peer.to_owned(),
            has_more: next_sequence < pair.messages.len() as u64,
            next_sequence,
            messages,
        }))
    }
}

fn caller<'a>(
    view: &NativeCommittedView,
    authority: &'a CallerAuthority,
    now: SystemTime,
) -> Result<&'a tilo_user_identity::DeviceCert, String> {
    if !view.hosted_here() {
        return Err("private conversations require trusted workspace host custody".into());
    }
    view.peer()
        .permissions(view.device().device_public_key, now)
        .map_err(|error| error.to_string())?;
    let CallerAuthority::Person { caller } = authority else {
        return Err("private conversations require authenticated person transport".into());
    };
    if !view.peer().certificates().iter().any(|cert| cert == caller) {
        return Err("private conversation caller certificate is not current".into());
    }
    let member = view
        .peer()
        .permissions(caller.device_public_key, now)
        .map_err(|error| error.to_string())?;
    if member.foreign {
        return Err("foreign devices cannot access person conversations".into());
    }
    if member.delegate.is_some() {
        return Err("delegated agents cannot read person conversations".into());
    }
    Ok(caller)
}

impl NativeWorkspaceOwner {
    /// Clear the person's old private conversations once their history is in
    /// their records: one publication, and nothing of them left in the
    /// owner's file. The field stays in the record, so a record written
    /// before the migration still reads.
    ///
    /// # Errors
    /// Refuses a caller that is not a current device of this person, or
    /// unavailable custody.
    pub fn clear_old_direct_conversations(
        &self,
        workspace: WorkspaceId,
        authority: &CallerAuthority,
        now: SystemTime,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(workspace, |view| {
            caller(view, authority, now)?;
            Ok(view.clone())
        })?;
        if candidate.record.old_direct_conversations.is_empty() {
            return Ok(());
        }
        candidate.record.old_direct_conversations = OldDirectConversations::default();
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("owner generation exhausted")?;
        self.publish_workspace(workspace, candidate, None)
    }

    /// One page of the conversation with the person the store names
    /// `peer_key`, as the old store holds it: the migration reads a person's
    /// history here before it is cleared. `Ok(None)` where the store holds no
    /// conversation with them.
    ///
    /// # Errors
    /// Refuses invalid cursors, a corrupt store, or unavailable custody.
    pub fn old_direct_conversation(
        &self,
        workspace: WorkspaceId,
        peer_key: &str,
        authority: &CallerAuthority,
        page: OldConversationPage,
        now: SystemTime,
    ) -> Result<Option<OldConversation>, String> {
        self.with_current(workspace, |view| {
            let caller = caller(view, authority, now)?;
            let store = &view.record.old_direct_conversations;
            store.validate()?;
            let me = person_store_key(caller.user_public_key);
            if peer_key == me {
                return Err("a conversation needs another person".into());
            }
            store.conversation(&me, peer_key, page)
        })
    }

    /// Keep `from_own` and then `from_peer`, as the old store held them, for
    /// `peer_public_key` (root hex): the fixture the import's tests read.
    ///
    /// # Errors
    /// Refuses a caller that is not a current device of this person, invalid
    /// keys, or unavailable custody.
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_old_direct_conversations(
        &self,
        workspace: WorkspaceId,
        peer_public_key: &str,
        from_own: &[&str],
        from_peer: &[&str],
        now: SystemTime,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(workspace, |view| {
            let cert = view.device().clone();
            caller(view, &CallerAuthority::Person { caller: cert }, now)?;
            Ok(view.clone())
        })?;
        let mut root = [0u8; 32];
        hex::decode_to_slice(peer_public_key, &mut root).map_err(|_| "invalid peer key")?;
        let own = person_store_key(candidate.record.device.user_public_key);
        let other = person_store_key(root);
        if own == other {
            return Err("a conversation needs another person".into());
        }
        let mut people = [own.clone(), other.clone()];
        people.sort();
        let received_at = i64::try_from(
            now.duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_secs(),
        )
        .map_err(|e| e.to_string())?;
        let id = format!(
            "direct_{}",
            blake3::hash(
                &serde_json::to_vec(&("private-person-pair-v1", workspace, &people))
                    .map_err(|e| e.to_string())?
            )
            .to_hex()
        );
        let store = &mut candidate.record.old_direct_conversations;
        let pair = store.pairs.entry(id).or_insert_with(|| Pair {
            workspace,
            people,
            messages: Vec::new(),
        });
        for (text, sender) in from_own
            .iter()
            .map(|text| (*text, &own))
            .chain(from_peer.iter().map(|text| (*text, &other)))
        {
            text_valid(text)?;
            if pair.messages.len() >= MAX_MESSAGES {
                return Err("private message capacity exhausted".into());
            }
            let sequence = (pair.messages.len() as u64).saturating_add(1);
            pair.messages.push(OldMessage {
                id: format!("dm_{}_{sequence}", blake3::hash(text.as_bytes()).to_hex()),
                sender: sender.clone(),
                sequence,
                received_at,
                text: (*text).to_owned(),
            });
        }
        candidate.record.old_direct_conversations.validate()?;
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("owner generation exhausted")?;
        self.publish_workspace(workspace, candidate, None)
    }
}

#[cfg(test)]
mod tests;
