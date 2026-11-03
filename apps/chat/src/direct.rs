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
//! The views keep calling [`ui::person_chat`], which this window answers
//! from the record ([`Records`] as its [`ui::person_chat::Native`]): the
//! same functions, the same contact and message shapes, a different source.
//! What only the session's owner can do stays on its door: announcing the
//! pair, and reading the old private store so the conversations kept there
//! are imported once ([`Records::import_old_store`]).
//!
//! A person is kept in the conversation under the key the private store has
//! always used for them ([`ui::person_chat::person_key`], the hash of their
//! root public key): authors, read marks and the views' peer ids are that
//! one spelling, so an imported message and a new one are told apart by
//! their author the same way.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use ui::framework::WorkspaceRecord;
use ui::tilo_resource::{self, ActionCx, Collection, Id, JsonSchema, Model, Refusal};

/// The app whose store keeps the conversations in a workspace.
pub const OWNER: &str = "so.tilo.chat";
/// The longest message text, in bytes: the door's own bound.
pub const MAX_TEXT_BYTES: usize = 16 * 1024;
/// The most messages a conversation keeps: what the door hands over whole.
pub const MAX_MESSAGES: usize = 2000;
/// The messages one import page asks the old store for.
const IMPORT_PAGE: usize = 100;
/// How many people one window imports from the old store per sync: the
/// import is a migration, not a reason to stall a frame.
const IMPORTS_PER_SYNC: usize = 4;

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

/// What the two people of a pair have said to each other, and how far each
/// of them has read.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct DirectConversation {
    /// What they said, oldest first, each under the id it was sent with.
    #[model(readonly)]
    pub messages: Collection<DirectMessage>,
    /// How far each person has read: the time of the last message they read,
    /// in milliseconds since the epoch, written by them alone.
    pub read: BTreeMap<String, i64>,
}

impl DirectConversation {
    /// The conversation as a view shows it, with `peer` as the other
    /// person (in the key a conversation keeps people under).
    #[must_use]
    pub fn conversation(&self, peer: &str) -> ui::person_chat::Conversation {
        let mut messages: Vec<_> = self
            .messages
            .iter()
            .map(|(id, message)| (id.clone(), message))
            .collect();
        messages.sort_by(|(_, a), (_, b)| a.at.cmp(&b.at));
        ui::person_chat::Conversation {
            peer: peer.to_owned(),
            messages: messages
                .into_iter()
                .map(|(id, message)| ui::person_chat::Message {
                    id: id.as_str().to_owned(),
                    author: message.author.clone(),
                    text: message.text.clone(),
                })
                .collect(),
        }
    }

    /// How many messages `me` has not read: the other person's messages
    /// newer than their read mark.
    #[must_use]
    pub fn unread(&self, me: &str) -> u32 {
        let read = self.read.get(me).copied().unwrap_or(i64::MIN);
        let count = self
            .messages
            .values()
            .filter(|message| message.author != me && message.at > read)
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }
}

/// The message id an invocation sends: the same invocation is the same
/// message, so a retry stores nothing twice.
#[must_use]
pub fn message_id(invocation: uuid::Uuid) -> String {
    format!("dm_{invocation}")
}

fn person(cx: &ActionCx) -> Result<String, Refusal> {
    cx.author()
        .filter(|author| !author.is_empty() && author.len() <= 4096)
        .map(|author| ui::person_chat::person_key(author))
        .ok_or_else(|| Refusal::new("a message is always someone's"))
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

#[ui::tilo_resource::resource(
    kind = "chat.direct",
    label = "Direct conversation",
    params = conversation,
)]
impl DirectConversation {
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
        if self.messages.len() >= MAX_MESSAGES {
            return Err(Refusal::new("the conversation holds too many messages"));
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

    /// Mark how far you have read.
    ///
    /// Only ever moves forward, and only for the person reading.
    #[action(housekeeping)]
    pub fn mark_read(
        &mut self,
        cx: &mut ActionCx,
        #[param("The time of the last message read")] seen: i64,
    ) -> Result<(), Refusal> {
        let me = person(cx)?;
        let mark = self.read.entry(me).or_insert(i64::MIN);
        if seen > *mark {
            *mark = seen;
        }
        Ok(())
    }

    /// Take in a conversation kept in the old private store.
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
            if self.messages.len() >= MAX_MESSAGES {
                return Err(Refusal::new("the conversation holds too many messages"));
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
    open: RefCell<BTreeMap<String, WorkspaceRecord<DirectConversation>>>,
    /// The people whose old private conversation is still to be imported.
    imports: RefCell<VecDeque<String>>,
    /// The pairs this person is in, as the workspace last proved them.
    pairs: RefCell<Option<Vec<ui::person_chat::Pair>>>,
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
            pairs: RefCell::new(None),
        })
    }

    /// The people the window may talk to, and the conversation each of them
    /// already has: what the direct message list shows.
    pub(crate) fn set_people(&self, people: Vec<Person>) {
        for person in &people {
            if !person.key.is_empty() && !self.imports.borrow().contains(&person.key) {
                self.imports.borrow_mut().push_back(person.key.clone());
            }
        }
        *self.people.borrow_mut() = people;
    }

    /// The pairs this person is in, asked of the workspace again: a device
    /// that never opened one learns it here, and opens it so it syncs. A
    /// pair the workspace proved exists: opening its record creates nothing
    /// nobody asked for.
    fn refresh_pairs(&self) {
        let Some(Ok(pairs)) = ui::person_chat::pairs().take() else {
            return;
        };
        let me = self.root();
        let mut pairs: Vec<_> = pairs
            .into_iter()
            .filter(|pair| pair.people.iter().any(|person| *person == me))
            .collect();
        pairs.sort_by(|a, b| a.pair.cmp(&b.pair));
        for pair in &pairs {
            if !self.open.borrow().contains_key(&pair.pair)
                && let Some(record) = self.handle.direct::<DirectConversation>(OWNER, &pair.pair)
            {
                self.open.borrow_mut().insert(pair.pair.clone(), record);
            }
        }
        *self.pairs.borrow_mut() = Some(pairs);
    }

    /// How many messages of `peer`'s conversation this person has not read.
    pub(crate) fn unread(&self, peer: &str) -> u32 {
        self.held(peer)
            .map_or(0, |record| record.get().unread(&self.me))
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
        collab_types::direct_pair_id(&self.workspace, &self.root(), root)
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

    /// The conversation with `peer`, opened (its record created where the
    /// workspace holds none) and followed change by change.
    fn record(&self, peer: &str) -> Result<WorkspaceRecord<DirectConversation>, String> {
        let person = self
            .person(peer)
            .ok_or_else(|| "no direct conversation with them here".to_owned())?;
        let pair = self.pair(&person.root)?;
        if let Some(record) = self.open.borrow().get(&pair) {
            return Ok(record.clone());
        }
        let record = self
            .handle
            .direct::<DirectConversation>(OWNER, &pair)
            .ok_or_else(|| "the conversation's record is unavailable".to_owned())?;
        self.open.borrow_mut().insert(pair, record.clone());
        Ok(record)
    }

    /// The conversation with `peer` as it stands, without opening one that
    /// is not open here.
    fn held(&self, peer: &str) -> Option<WorkspaceRecord<DirectConversation>> {
        let person = self.person(peer)?;
        let pair = self.pair(&person.root).ok()?;
        if let Some(record) = self.open.borrow().get(&pair) {
            return Some(record.clone());
        }
        if !self.handle.holds_direct::<DirectConversation>(OWNER, &pair) {
            return None;
        }
        let record = self.handle.direct::<DirectConversation>(OWNER, &pair)?;
        self.open.borrow_mut().insert(pair, record.clone());
        Some(record)
    }

    /// Announce the pair with `peer` to the workspace, so that its record is
    /// the two people's: the door holds the device key that proves it.
    fn announce(&self, peer: &str) -> Result<(), String> {
        match ui::person_chat::announce(peer).take() {
            Some(Ok(())) => Ok(()),
            Some(Err(error)) => Err(error),
            None => Err("the pair is being announced".into()),
        }
    }

    /// Whether this person has read up to the newest message of `peer`'s
    /// conversation, marking them read when not: what an open conversation
    /// does as it shows what came in.
    pub(crate) fn mark_read(&self, peer: &str) {
        let Some(record) = self.held(peer) else {
            return;
        };
        let state = record.get();
        let read = state.read.get(&self.me).copied().unwrap_or(i64::MIN);
        let newest = state
            .messages
            .values()
            .filter(|message| message.author != self.me)
            .map(|message| message.at)
            .max();
        if let Some(newest) = newest
            && newest > read
        {
            let _ = record.invoke(
                &DirectConversation::MARK_READ.with(conversation::MarkRead { seen: newest }),
            );
        }
    }

    /// Import the conversation with `peer` from the old private store, page
    /// by page, into its record. Answers whether it had nothing more to take
    /// in.
    fn import(&self, peer: &str) -> bool {
        let mut after = 0;
        let mut imported = Vec::new();
        loop {
            let page = match ui::person_chat::import_archive(peer, after).take() {
                Some(Ok(page)) => page,
                // The old store has nothing more here: done, whatever came.
                Some(Err(_)) | None => break,
            };
            after = page.next_sequence;
            if page.messages.is_empty() && !page.has_more {
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
            return true;
        }
        let Ok(record) = self.record(peer) else {
            return false;
        };
        let _ = self.announce(peer);
        record
            .invoke(&DirectConversation::IMPORT.with(conversation::Import { messages: imported }))
            .is_ok()
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

impl ui::person_chat::Native for Records {
    fn contacts(&self) -> Vec<ui::person_chat::Contact> {
        self.refresh_pairs();
        let held: Vec<String> = self
            .open
            .borrow()
            .iter()
            .filter(|(_, record)| !record.get().messages.is_empty())
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
                    .is_ok_and(|pair| held.iter().any(|held| *held == pair));
                ui::person_chat::Contact {
                    public_key: person.key.clone(),
                    display_name: person.name.clone(),
                    conversation,
                }
            })
            .collect()
    }

    fn open(&self, peer: &str) -> Result<ui::person_chat::Conversation, String> {
        let record = self.record(peer)?;
        // Whoever opens a conversation makes its record the pair's: the one
        // write only the session's owner can sign.
        self.announce(peer)?;
        Ok(record.get().conversation(peer))
    }

    fn read(&self, peer: &str) -> Result<ui::person_chat::Conversation, String> {
        match self.held(peer) {
            Some(record) => Ok(record.get().conversation(peer)),
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
        let record = self.record(peer)?;
        self.announce(peer)?;
        record
            .invoke(&DirectConversation::SEND.with(conversation::Send {
                id: message_id(invocation),
                text: text.to_owned(),
            }))
            .map_err(|error| error.to_string())?;
        Ok(record.get().conversation(peer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn send(device: &mut Replica<DirectConversation>, id: &str, text: &str) -> Result<(), String> {
        device
            .apply(DirectConversation::SEND.with(conversation::Send {
                id: id.to_owned(),
                text: text.to_owned(),
            }))
            .map(drop)
            .map_err(|error| error.to_string())
    }

    #[test]
    fn a_sent_message_names_its_writer_and_comes_once() {
        let mut ada = device(ADA, 1_000);
        send(&mut ada, "dm_1", "hi").unwrap();
        send(&mut ada, "dm_1", "hi").unwrap();
        let state = ada.state().unwrap();
        assert_eq!(state.messages.len(), 1);
        let (id, message) = state.messages.iter().next().unwrap();
        assert_eq!(id.as_str(), "dm_1");
        assert_eq!(message.author, ui::person_chat::person_key(ADA));
        assert_eq!(message.text, "hi");
        assert_eq!(message.at, 1_000);
    }

    #[test]
    fn an_empty_or_oversized_message_is_refused() {
        let mut ada = device(ADA, 1_000);
        assert!(send(&mut ada, "dm_1", "  ").is_err());
        assert!(send(&mut ada, "dm_2", &"x".repeat(MAX_TEXT_BYTES + 1)).is_err());
        assert!(send(&mut ada, "no spaces", "hi").is_err());
        assert!(ada.state().unwrap().messages.is_empty());
    }

    #[test]
    fn two_devices_keep_one_conversation_and_their_own_read_marks() {
        let mut ada = device(ADA, 1_000);
        // Both devices hold the record the pair's genesis made, as they do
        // when each opened it from what the workspace keeps.
        let mut bob = second_device(&ada, BOB, 2_000);
        send(&mut ada, "dm_1", "one").unwrap();
        send(&mut bob, "dm_2", "two").unwrap();
        ada.doc_mut().merge(&mut bob.doc().clone()).unwrap();
        let (ada_key, bob_key) = (
            ui::person_chat::person_key(ADA),
            ui::person_chat::person_key(BOB),
        );
        let state = ada.state().unwrap();
        assert_eq!(state.conversation(&bob_key).messages.len(), 2);
        // Each has said one thing, and read neither.
        assert_eq!(state.unread(&ada_key), 1);
        assert_eq!(state.unread(&bob_key), 1);
        // Each person's own mark, written where they are.
        ada.set_now_ms(3_000);
        ada.apply(DirectConversation::MARK_READ.with(conversation::MarkRead { seen: 2_000 }))
            .unwrap();
        bob.set_now_ms(3_000);
        bob.apply(DirectConversation::MARK_READ.with(conversation::MarkRead { seen: 2_000 }))
            .unwrap();
        bob.doc_mut().merge(&mut ada.doc_mut().clone()).unwrap();
        let state = bob.state().unwrap();
        assert_eq!(state.unread(&ada_key), 0);
        assert_eq!(state.unread(&bob_key), 0);
        assert_eq!(state.read.len(), 2);
    }

    #[test]
    fn a_read_mark_never_moves_back() {
        let mut ada = device(ADA, 1_000);
        let seen = DirectConversation::MARK_READ.with(conversation::MarkRead { seen: 2_000 });
        ada.apply(seen.clone()).unwrap();
        ada.apply(DirectConversation::MARK_READ.with(conversation::MarkRead { seen: 1_000 }))
            .unwrap();
        assert_eq!(
            ada.state().unwrap().read[&ui::person_chat::person_key(ADA)],
            2_000
        );
        assert_eq!(seen.id, DirectConversation::MARK_READ.id);
    }

    #[test]
    fn an_imported_message_is_the_same_message_imported_once() {
        let mut ada = device(ADA, 1_000);
        let stored = StoredMessage {
            id: "dm_old".into(),
            author: ui::person_chat::person_key(BOB),
            text: "before".into(),
            at: 1,
        };
        let import = |ada: &mut Replica<DirectConversation>| {
            ada.apply(DirectConversation::IMPORT.with(conversation::Import {
                messages: vec![stored.clone()],
            }))
            .map(drop)
            .map_err(|error| error.to_string())
        };
        import(&mut ada).unwrap();
        import(&mut ada).unwrap();
        let state = ada.state().unwrap();
        assert_eq!(state.messages.len(), 1);
        assert_eq!(
            state.messages.get(&Id::from_raw("dm_old")).unwrap().text,
            "before"
        );
    }

    #[test]
    fn a_stored_message_nobody_holds_in_the_store_is_not_imported() {
        let mut ada = device(ADA, 1_000);
        let bad = StoredMessage {
            id: "dm_old".into(),
            author: "not a person".into(),
            text: "before".into(),
            at: 1,
        };
        assert!(
            ada.apply(DirectConversation::IMPORT.with(conversation::Import {
                messages: vec![bad],
            }))
            .is_err()
        );
        assert!(ada.state().unwrap().messages.is_empty());
    }

    #[test]
    fn a_conversation_reads_oldest_first() {
        let mut ada = device(ADA, 1_000);
        send(&mut ada, "dm_2", "second").unwrap();
        ada.set_now_ms(500);
        send(&mut ada, "dm_1", "first").unwrap();
        let shown = ada.state().unwrap().conversation("peer");
        assert_eq!(shown.peer, "peer");
        assert_eq!(
            shown
                .messages
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
    }
}
