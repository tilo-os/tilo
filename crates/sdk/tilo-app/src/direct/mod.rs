//! A conversation, as the record the two people it is between share: the one
//! resource every app reads and writes it as.
//!
//! A conversation lives in an ordinary record of the workspace
//! (`direct/<pair>`, kept by [`OWNER`]), named by the two people it is
//! between and decided by the workspace's policy like any other record of the
//! app: the workspace's grants on its owner's node say who reads and changes
//! it. [`OWNER`] is the record's owner for every app that
//! opens it, so a conversation read in one app is the same record, under the
//! same id and the same stored names, as in any other.
//!
//! The record itself is small: who the pair is, how far each of them read
//! ([`DirectConversation::marks`]), and the messages the pair wrote before
//! they moved into parts ([`DirectConversation::messages`]). The messages
//! are the pair's own chain of parts beside that record ([`parts`],
//! [`parts::DirectPart`]): 2000 messages or 1 MiB of text each, as many as
//! the conversation needs.
//!
//! A window reads and writes them through [`Records`], which opens the
//! record through the framework's workspace handle
//! ([`crate::framework::WorkspaceHandle`]) so a message another device sends
//! wakes it like any other change of one of the app's records. What only the
//! session's owner can do stays on its door ([`crate::person_chat`]): reading
//! the old private store so the conversations kept there are imported once
//! ([`Records::import_old_store`]), and emptying it.
//!
//! A person is kept in the conversation under the key the private store has
//! always used for them ([`crate::person_chat::person_key`], the hash of their
//! root public key): authors, read marks and the views' peer ids are that
//! one spelling, so an imported message and a new one are told apart by
//! their author the same way.

pub mod parts;
mod records;

use std::collections::BTreeMap;

use tilo_resource::{self, ActionCx, Collection, Model, Refusal};

use parts::{DirectMessage, DirectReadMark};

pub use records::{Person, Records};

/// The app whose store keeps a pair's conversation in a workspace: the
/// owner of these records for every app that opens them, so all of them
/// open, sync and prove the same records. Never another app's id.
///
/// An app that reads the conversation passes this owner to the workspace
/// handle, and declares the kinds the records name (`chat.direct` and
/// `chat.direct-messages`) in its manifest.
pub const OWNER: &str = "so.tilo.chat";

/// What the two people of a pair have said to each other, and how far each
/// of them has read.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct DirectConversation {
    /// What they said before the conversation moved to parts ([`parts`]):
    /// kept readable, never written again, each under the id it was sent
    /// with.
    #[model(readonly)]
    pub messages: Collection<DirectMessage>,
    /// How far each person had read under the one-record conversation: the
    /// time of the last message they read, in milliseconds since the epoch,
    /// written by them alone. Read beside [`Self::marks`] so what a person
    /// read before the parts came is not unread again.
    pub read: BTreeMap<String, i64>,
    /// How far each person has read ([`DirectReadMark`]), written by them
    /// alone and only ever further.
    pub marks: BTreeMap<String, DirectReadMark>,
}

impl DirectConversation {
    /// How many messages of `peer`'s the person `me` has not read: those of
    /// the messages kept in the record itself after their time mark, and,
    /// through `conversation`, those of the parts after their chain mark.
    #[must_use]
    pub fn unread(&self, conversation: &parts::Conversation, me: &str, peer: &str) -> u32 {
        let read = self.read.get(me).copied().unwrap_or(i64::MIN);
        let kept = self
            .messages
            .values()
            .filter(|message| message.author != me && message.at > read)
            .count();
        let parts = conversation.unread(peer, self.marks.get(me).copied().unwrap_or_default());
        u32::try_from(kept)
            .unwrap_or(u32::MAX)
            .saturating_add(parts)
    }
}

fn person(cx: &ActionCx) -> Result<String, Refusal> {
    cx.author()
        .filter(|author| !author.is_empty() && author.len() <= 4096)
        .map(crate::person_chat::person_key)
        .ok_or_else(|| Refusal::new("a message is always someone's"))
}

#[tilo_resource::resource(
    kind = "chat.direct",
    label = "Direct conversation",
    params = conversation,
)]
impl DirectConversation {
    /// Mark how far you have read.
    ///
    /// Only ever moves forward, and only for the person reading.
    #[action(housekeeping)]
    pub fn mark_read(
        &mut self,
        cx: &mut ActionCx,
        #[param("The part they read into")] part: i64,
        #[param("Messages of the other person they saw there")] seen: i64,
    ) -> Result<(), Refusal> {
        if part < 0 || seen < 0 {
            return Err(Refusal::new("a read mark is a place in the conversation"));
        }
        let me = person(cx)?;
        let mark = DirectReadMark { part, seen };
        let held = self.marks.entry(me.clone()).or_default();
        if mark > *held {
            *held = mark;
        }
        // Reading into the parts reads what the record itself kept before
        // them: it is older than anything a part holds.
        let newest = self
            .messages
            .values()
            .filter(|message| message.author != me)
            .map(|message| message.at)
            .max();
        if let Some(newest) = newest {
            let read = self.read.entry(me).or_insert(i64::MIN);
            if newest > *read {
                *read = newest;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod acceptance;

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_automerge::ApplyIncoming as _;
    use tilo_resource::Replica;

    const ADA: &str = "adadadadadadadadadadadadadadadadadadadadadadadadadadadadadadadad";
    const BOB: &str = "b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0";

    /// One device of `author`, its clock at `at`.
    fn device(author: &str, at: i64) -> Replica<DirectConversation> {
        let mut replica = Replica::<DirectConversation>::new().as_invoker(author);
        replica.set_now_ms(at);
        replica
    }

    /// A second device that already holds `first`'s record, its own clock at
    /// `at` and `author` acting on it.
    fn second_device(
        first: &Replica<DirectConversation>,
        author: &str,
        at: i64,
    ) -> Replica<DirectConversation> {
        let mut replica =
            Replica::<DirectConversation>::from_doc(first.doc().clone()).as_invoker(author);
        replica
            .doc_mut()
            .set_actor(automerge::ActorId::from(author.as_bytes()));
        replica.set_now_ms(at);
        replica
    }

    fn mark(device: &mut Replica<DirectConversation>, part: i64, seen: i64) -> Result<(), String> {
        device
            .apply(DirectConversation::MARK_READ.with(conversation::MarkRead { part, seen }))
            .map(drop)
            .map_err(|error| error.to_string())
    }

    #[test]
    fn two_devices_keep_their_own_read_marks_forward_only() {
        let mut ada = device(ADA, 1_000);
        let bob = second_device(&ada, BOB, 2_000);
        ada.doc_mut()
            .merge_incoming(&mut bob.doc().clone())
            .unwrap();
        mark(&mut ada, 0, 3).unwrap();
        mark(&mut ada, 0, 2).unwrap();
        let state = ada.state().unwrap();
        assert_eq!(
            state.marks[&crate::person_chat::person_key(ADA)],
            DirectReadMark { part: 0, seen: 3 }
        );
        assert!(
            !state
                .marks
                .contains_key(&crate::person_chat::person_key(BOB))
        );
    }

    #[test]
    fn a_read_mark_before_the_conversation_is_refused() {
        let mut ada = device(ADA, 1_000);
        assert!(mark(&mut ada, -1, 0).is_err());
        assert!(mark(&mut ada, 0, -1).is_err());
        assert!(ada.state().unwrap().marks.is_empty());
    }

    #[test]
    fn a_kept_message_is_unread_until_the_person_reads_into_the_parts() {
        // A record written before the conversation moved to parts: an empty
        // chain here, and one message of the other person kept in the
        // record.
        let mut ada = device(ADA, 1_000);
        let me = crate::person_chat::person_key(ADA);
        let peer = crate::person_chat::person_key(BOB);
        let empty = parts::Conversation::new(Vec::new());
        assert_eq!(ada.state().unwrap().unread(&empty, &me, &peer), 0);
        // The same record with a kept message is what a device that ran the
        // one-record conversation holds; marking its read moves the time
        // mark with the chain mark, so what was read there stays read.
        mark(&mut ada, 0, 0).unwrap();
        let state = ada.state().unwrap();
        assert_eq!(state.unread(&empty, &me, &peer), 0);
        assert_eq!(state.marks[&me], DirectReadMark { part: 0, seen: 0 });
        assert_eq!(state.read, BTreeMap::new());
    }
}
