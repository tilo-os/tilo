//! A direct conversation as the two people's own record.
//!
//! Chat's conversation with another person is theirs, not the workspace's:
//! it lives in the record the pair's two people share
//! (`direct/<pair>`, kept by [`OWNER`] in the workspace), which the
//! workspace proves for the two of them and for no one else's devices. What
//! a window reads and writes here is that record, opened through the
//! framework's workspace handle ([`ui::framework::WorkspaceHandle`]) so a
//! message another device sends wakes the window like any other change of
//! its own record.
//!
//! The record itself is small: who the pair is, how far each of them read
//! ([`DirectConversation::marks`]), and the messages the pair wrote before
//! they moved into parts ([`DirectConversation::messages`]). The messages
//! are the pair's own chain of parts beside that record
//! ([`crate::direct_parts`], [`DirectPart`]): 2000 messages or 1 MiB of text
//! each, as many as the conversation needs.
//!
//! The views keep calling [`ui::person_chat`], which this window answers
//! from the records ([`Records`] as its [`ui::person_chat::Native`]): the
//! same functions, the same contact and message shapes, a different source.
//! What only the session's owner can do stays on its door: announcing the
//! pair, reading which pairs this person is in, and reading the old private
//! store so the conversations kept there are imported once
//! ([`Records::import_old_store`]).
//!
//! A person is kept in the conversation under the key the private store has
//! always used for them ([`ui::person_chat::person_key`], the hash of their
//! root public key): authors, read marks and the views' peer ids are that
//! one spelling, so an imported message and a new one are told apart by
//! their author the same way.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

use ui::framework::{Chain, WorkspaceRecord};
use ui::tilo_resource::{self, ActionCx, Collection, Model, Refusal};

use crate::direct_parts::{
    self, DirectMessage, DirectPart, DirectReadMark, PART_MESSAGES, PART_TEXT_BYTES, StoredMessage,
};

/// The app whose store keeps the conversations in a workspace.
pub const OWNER: &str = "so.tilo.chat";
/// The longest message text, in bytes: the door's own bound.
pub const MAX_TEXT_BYTES: usize = 16 * 1024;
/// How many people one window imports from the old store per sync: the
/// import is a migration, not a reason to stall a frame.
const IMPORTS_PER_SYNC: usize = 4;

/// What the two people of a pair have said to each other, and how far each
/// of them has read.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct DirectConversation {
    /// What they said before the conversation moved to parts
    /// ([`crate::direct_parts`]): kept readable, never written again, each
    /// under the id it was sent with.
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
    pub fn unread(&self, conversation: &direct_parts::Conversation, me: &str, peer: &str) -> u32 {
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
        .map(ui::person_chat::person_key)
        .ok_or_else(|| Refusal::new("a message is always someone's"))
}

#[ui::tilo_resource::resource(
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

/// A person this window may talk to: their root public key as the workspace
/// spells it, their name, and the key and name a conversation keeps them
/// under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Person {
    /// Their root public key, in the canonical hex the workspace names them
    /// by: what a pair's record id is named with.
    pub root: String,
    /// The key a conversation keeps them under.
    pub key: String,
    pub name: String,
}

/// One conversation held here: the pair's record and the chain of parts its
/// messages are in.
#[derive(Clone)]
struct Open {
    record: WorkspaceRecord<DirectConversation>,
    chain: Chain<DirectPart>,
}

/// What a window's direct conversations read and write: the records the
/// pair's two people share, in the window's workspace.
pub(crate) struct Records {
    handle: ui::framework::WorkspaceHandle,
    /// The workspace as a pair's record id is named with.
    workspace: String,
    /// This person, under the key a conversation keeps them.
    me: String,
    /// The people of the workspace, as the window last read them.
    people: RefCell<Vec<Person>>,
    /// The conversations open here, by pair, so a send opens none twice.
    open: RefCell<BTreeMap<String, Open>>,
    /// The people whose old private conversation is still to be imported.
    imports: RefCell<VecDeque<String>>,
    /// The people whose old conversation was taken in already.
    imported: RefCell<BTreeSet<String>>,
    /// The workspace's answer on which pairs this person is in, and the
    /// time it answered for: `None` until the first answer.
    pairs: RefCell<Option<Vec<ui::person_chat::Pair>>>,
    /// The pair list's generation that answer was asked at
    /// ([`ui::person_chat::pairs_generation`]): the workspace announces a
    /// governance change, and the list is asked again only then.
    pairs_generation: Cell<Option<u64>>,
    /// The pairs this window announced, so a send asks the workspace once.
    announced: RefCell<BTreeSet<String>>,
}

impl Records {
    pub(crate) fn new(
        handle: ui::framework::WorkspaceHandle,
        workspace: String,
        me: &str,
    ) -> Rc<Self> {
        Rc::new(Self {
            handle,
            workspace,
            me: ui::person_chat::person_key(me),
            people: RefCell::default(),
            open: RefCell::default(),
            imports: RefCell::default(),
            imported: RefCell::default(),
            pairs: RefCell::new(None),
            pairs_generation: Cell::default(),
            announced: RefCell::default(),
        })
    }

    /// The people the window may talk to, and the conversation each of them
    /// already has: what the direct message list shows.
    pub(crate) fn set_people(&self, people: Vec<Person>) {
        for person in &people {
            if !person.key.is_empty()
                && !self.imports.borrow().contains(&person.key)
                && !self.imported.borrow().contains(&person.key)
            {
                self.imports.borrow_mut().push_back(person.key.clone());
            }
        }
        *self.people.borrow_mut() = people;
    }

    /// The pairs this person is in, asked of the workspace again when its
    /// governance changed ([`ui::person_chat::pairs_generation`]): a device
    /// that never opened one learns it here, and opens it so it syncs. A
    /// pair the workspace proved exists: opening its record creates nothing
    /// nobody asked for.
    fn refresh_pairs(&self) {
        let generation = ui::person_chat::pairs_generation();
        if self.pairs.borrow().is_some() && self.pairs_generation.get() == Some(generation) {
            return;
        }
        let Some(Ok(pairs)) = ui::person_chat::pairs().take() else {
            return;
        };
        self.pairs_generation.set(Some(generation));
        let me = self.root();
        let mut pairs: Vec<_> = pairs
            .into_iter()
            .filter(|pair| pair.people.contains(&me))
            .collect();
        pairs.sort_by_key(|pair| pair.pair.clone());
        for pair in &pairs {
            if !self.open.borrow().contains_key(&pair.pair)
                && let Some(open) = self.opened(&pair.pair)
            {
                self.open.borrow_mut().insert(pair.pair.clone(), open);
            }
        }
        *self.pairs.borrow_mut() = Some(pairs);
    }

    /// How many messages of `peer`'s conversation this person has not read.
    pub(crate) fn unread(&self, peer: &str) -> u32 {
        self.held(peer).map_or(0, |open| {
            open.record
                .get()
                .unread(&self.conversation(&open), &self.me, peer)
        })
    }

    /// This person's root public key.
    fn root(&self) -> String {
        self.people
            .borrow()
            .iter()
            .find(|person| person.key == self.me)
            .map_or_else(String::new, |person| person.root.clone())
    }

    /// The pair id of the conversation with `root`.
    fn pair(&self, root: &str) -> Result<String, String> {
        ui::person_chat::direct_pair_id(&self.workspace, &self.root(), root)
            .ok_or_else(|| "no direct conversation in this workspace".to_owned())
    }

    /// The person a conversation keeps under `peer`.
    fn person(&self, peer: &str) -> Option<Person> {
        self.people
            .borrow()
            .iter()
            .find(|person| person.key == peer)
            .cloned()
    }

    /// `pair`'s record and its parts, opened (the record created where the
    /// workspace holds none) and followed change by change.
    fn opened(&self, pair: &str) -> Option<Open> {
        let record = self.handle.direct::<DirectConversation>(OWNER, pair)?;
        let chain = self.handle.chain_beside::<DirectPart>(OWNER, &record)?;
        Some(Open { record, chain })
    }

    /// The conversation with `peer`, opened.
    fn record(&self, peer: &str) -> Result<Open, String> {
        let person = self
            .person(peer)
            .ok_or_else(|| "no direct conversation with them here".to_owned())?;
        let pair = self.pair(&person.root)?;
        if let Some(open) = self.open.borrow().get(&pair) {
            return Ok(open.clone());
        }
        let open = self
            .opened(&pair)
            .ok_or_else(|| "the conversation's record is unavailable".to_owned())?;
        self.open.borrow_mut().insert(pair, open.clone());
        Ok(open)
    }

    /// The conversation with `peer` as it stands, without opening one that
    /// is not open here.
    fn held(&self, peer: &str) -> Option<Open> {
        let person = self.person(peer)?;
        let pair = self.pair(&person.root).ok()?;
        if let Some(open) = self.open.borrow().get(&pair) {
            return Some(open.clone());
        }
        if !self.handle.holds_direct::<DirectConversation>(OWNER, &pair) {
            return None;
        }
        let open = self.opened(&pair)?;
        self.open.borrow_mut().insert(pair, open.clone());
        Some(open)
    }

    /// The conversation with `peer` as a view shows it: the messages the
    /// record kept before the parts, and every message of the parts held
    /// here, oldest first.
    fn conversation(&self, open: &Open) -> direct_parts::Conversation {
        direct_parts::Conversation::new(open.chain.parts())
    }

    /// The conversation with `peer` as a view shows it.
    fn shown(&self, open: &Open, peer: &str) -> ui::person_chat::Conversation {
        let state = open.record.get();
        let parts = self.conversation(open);
        let mut messages: BTreeMap<String, (i64, ui::person_chat::Message)> = state
            .messages
            .iter()
            .map(|(id, message)| {
                (
                    id.as_str().to_owned(),
                    (
                        message.at,
                        ui::person_chat::Message {
                            id: id.as_str().to_owned(),
                            author: message.author.clone(),
                            text: message.text.clone(),
                        },
                    ),
                )
            })
            .collect();
        // A part's message is the message: the same id imported again is
        // not shown twice.
        for (id, message) in parts.messages() {
            messages.insert(
                id.as_str().to_owned(),
                (
                    message.at,
                    ui::person_chat::Message {
                        id: id.as_str().to_owned(),
                        author: message.author.clone(),
                        text: message.text.clone(),
                    },
                ),
            );
        }
        let mut messages: Vec<(i64, ui::person_chat::Message)> = messages.into_values().collect();
        messages.sort_by_key(|(at, _)| *at);
        ui::person_chat::Conversation {
            peer: peer.to_owned(),
            messages: messages.into_iter().map(|(_, message)| message).collect(),
        }
    }

    /// Whether the conversation with `peer` says anything yet.
    fn speaks(open: &Open) -> bool {
        !open.record.get().messages.is_empty()
            || open
                .chain
                .parts()
                .iter()
                .any(|(_, part)| !part.messages.is_empty())
    }

    /// Announce the pair with `peer` to the workspace, so that its record is
    /// the two people's: the door holds the device key that proves it, and
    /// names the other person by their root public key.
    fn announce(&self, peer: &str) -> Result<(), String> {
        let person = self
            .person(peer)
            .ok_or_else(|| "no direct conversation with them here".to_owned())?;
        let pair = self.pair(&person.root)?;
        if self.announced.borrow().contains(&pair) {
            return Ok(());
        }
        match ui::person_chat::announce(&person.root).take() {
            Some(Ok(())) => {
                self.announced.borrow_mut().insert(pair);
                Ok(())
            }
            Some(Err(error)) => Err(error),
            None => Err("the pair is being announced".into()),
        }
    }

    /// Whether this person has read up to the newest message of `peer`'s
    /// conversation, marking them read when not: what an open conversation
    /// does as it shows what came in.
    pub(crate) fn mark_read(&self, peer: &str) {
        let Some(open) = self.held(peer) else {
            return;
        };
        let mark = self.conversation(&open).read_to_end(peer);
        let held = open
            .record
            .get()
            .marks
            .get(&self.me)
            .copied()
            .unwrap_or_default();
        if mark <= held {
            return;
        }
        let _ = open
            .record
            .invoke(&DirectConversation::MARK_READ.with(conversation::MarkRead {
                part: mark.part,
                seen: mark.seen,
            }));
    }

    /// Import the conversation with `peer` from the old private store, page
    /// by page, into the parts of its record. Answers whether it had nothing
    /// more to take in.
    fn import(&self, peer: &str) -> bool {
        let mut after = 0;
        let mut imported = Vec::new();
        loop {
            // The old store has nothing more here: done, whatever came.
            let Some(Ok(page)) = ui::person_chat::import_archive(peer, after).take() else {
                break;
            };
            after = page.next_sequence;
            if page.messages.is_empty() {
                break;
            }
            imported.extend(page.messages.into_iter().map(|message| StoredMessage {
                id: message.id,
                author: message.author,
                text: message.text,
                at: message.at,
            }));
            if !page.has_more {
                break;
            }
        }
        if imported.is_empty() {
            self.imported.borrow_mut().insert(peer.to_owned());
            return true;
        }
        let Ok(open) = self.announce(peer).and_then(|()| self.record(peer)) else {
            return false;
        };
        // A part holds 2000 messages or 1 MiB of them, so a long
        // conversation crosses into as many parts as it fills, and the
        // part after it is started the same way a message that finds the
        // head full does.
        let mut rest = imported;
        while !rest.is_empty() {
            let conversation = self.conversation(&open);
            let (room, text_room) = match conversation.parts().last() {
                Some((head, part)) if conversation.post_part() == *head => (
                    PART_MESSAGES.saturating_sub(part.messages.len()).max(1),
                    PART_TEXT_BYTES.saturating_sub(part.text_bytes()).max(1),
                ),
                _ => (PART_MESSAGES, PART_TEXT_BYTES),
            };
            let tail = imported_room(&mut rest, room, text_room);
            let chunk = std::mem::take(&mut rest);
            rest = tail;
            let taken = open
                .chain
                .invoke(
                    &DirectPart::IMPORT.with(direct_parts::direct_part::Import { messages: chunk }),
                )
                .is_ok();
            if !taken {
                return false;
            }
        }
        self.imported.borrow_mut().insert(peer.to_owned());
        true
    }

    /// Import a few conversations the old private store still holds, so a
    /// person's history comes over once without stalling a frame.
    pub(crate) fn import_old_store(&self) {
        for _ in 0..IMPORTS_PER_SYNC {
            let Some(peer) = self.imports.borrow_mut().pop_front() else {
                return;
            };
            if !self.import(&peer) {
                // Its record could not be written yet: ask again next sync.
                self.imports.borrow_mut().push_back(peer);
            }
        }
    }
}

/// Split off as many of `rest` as fit in a part with `room` messages and
/// `text_room` bytes left: at least one message, whatever it says.
fn imported_room(
    rest: &mut Vec<StoredMessage>,
    room: usize,
    text_room: usize,
) -> Vec<StoredMessage> {
    let mut take = 0;
    let mut bytes: usize = 0;
    while take < rest.len() && take < room {
        let len = rest[take].text.len();
        if take > 0 && bytes.saturating_add(len) > text_room {
            break;
        }
        bytes = bytes.saturating_add(len);
        take += 1;
    }
    rest.split_off(take)
}

impl ui::person_chat::Native for Records {
    fn contacts(&self) -> Vec<ui::person_chat::Contact> {
        self.refresh_pairs();
        let held: Vec<String> = self
            .open
            .borrow()
            .iter()
            .filter(|(_, open)| Records::speaks(open))
            .map(|(pair, _)| pair.clone())
            .collect();
        let me = self.me.clone();
        self.people
            .borrow()
            .iter()
            .filter(|person| person.key != me)
            .map(|person| {
                let conversation = self
                    .pair(&person.root)
                    .is_ok_and(|pair| held.contains(&pair));
                ui::person_chat::Contact {
                    public_key: person.key.clone(),
                    display_name: person.name.clone(),
                    conversation,
                }
            })
            .collect()
    }

    fn open(&self, peer: &str) -> Result<ui::person_chat::Conversation, String> {
        // Whoever opens a conversation makes its record the pair's: the one
        // write only the session's owner can sign. Before the record is
        // opened here, so this window writes nothing an unproved pair owns.
        self.announce(peer)?;
        let open = self.record(peer)?;
        // The whole conversation is what a view opens: every earlier part is
        // asked for, and arrives change by change as the workspace sends it.
        for (index, _) in open.chain.parts() {
            let _ = open.chain.hold(index);
        }
        Ok(self.shown(&open, peer))
    }

    fn read(&self, peer: &str) -> Result<ui::person_chat::Conversation, String> {
        match self.held(peer) {
            Some(open) => Ok(self.shown(&open, peer)),
            // No conversation here yet is an empty one, not a refusal.
            None if self.person(peer).is_some() => Ok(ui::person_chat::Conversation {
                peer: peer.to_owned(),
                messages: Vec::new(),
            }),
            None => Err("no direct conversation with them here".into()),
        }
    }

    fn send(
        &self,
        peer: &str,
        text: &str,
        invocation: uuid::Uuid,
    ) -> Result<ui::person_chat::Conversation, String> {
        let open = self.announce(peer).and_then(|()| self.record(peer))?;
        open.chain
            .invoke(&DirectPart::SEND.with(direct_parts::direct_part::Send {
                id: direct_parts::message_id(invocation),
                text: text.to_owned(),
            }))
            .map_err(|error| error.to_string())?;
        Ok(self.shown(&open, peer))
    }
}

#[cfg(test)]
mod acceptance;

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_automerge::ApplyIncoming as _;
    use ui::tilo_resource::Replica;

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
            .set_actor(ui::automerge::ActorId::from(author.as_bytes()));
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
            state.marks[&ui::person_chat::person_key(ADA)],
            DirectReadMark { part: 0, seen: 3 }
        );
        assert!(!state.marks.contains_key(&ui::person_chat::person_key(BOB)));
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
        let me = ui::person_chat::person_key(ADA);
        let peer = ui::person_chat::person_key(BOB);
        let empty = direct_parts::Conversation::new(Vec::new());
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
