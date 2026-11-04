//! The record a conversation lives in, read and written for an app's views.
//!
//! A window's views ask here; what they read is the conversation's record
//! and its chain of parts ([`super::DirectConversation`], [`super::parts`]),
//! opened through the framework's workspace handle so a message another
//! device sends wakes the window like any other change of one of the app's
//! records. The record is an ordinary record of the workspace, named by the
//! two people it is between, and the workspace's grants on the app decide
//! every change to it.

#[cfg(not(target_arch = "wasm32"))]
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::BTreeMap;
#[cfg(not(target_arch = "wasm32"))]
use std::collections::{BTreeSet, VecDeque};
use std::rc::Rc;

use crate::framework::{Chain, WorkspaceRecord};

use super::parts::{self, DirectPart};
#[cfg(not(target_arch = "wasm32"))]
use super::parts::{PART_MESSAGES, PART_TEXT_BYTES, StoredMessage};
use super::{DirectConversation, OWNER, conversation};

/// How many people one window imports from the old store per sync: the
/// import is a migration, not a reason to stall a frame.
#[cfg(not(target_arch = "wasm32"))]
const IMPORTS_PER_SYNC: usize = 4;

/// A person this window may talk to: their root public key as the workspace
/// spells it, their name, and the key and name a conversation keeps them
/// under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
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
pub struct Records {
    handle: crate::framework::WorkspaceHandle,
    /// The workspace as a pair's record id is named with.
    workspace: String,
    /// This person's root public key, as the workspace names them: what a
    /// pair's record id is named with.
    root: String,
    /// This person, under the key a conversation keeps them.
    me: String,
    /// The people of the workspace, as the window last read them.
    people: RefCell<Vec<Person>>,
    /// The conversations open here, by pair, so a send opens none twice.
    open: RefCell<BTreeMap<String, Open>>,
    /// The people whose old private conversation is still to be imported:
    /// only a device hosting the workspace holds one.
    #[cfg(not(target_arch = "wasm32"))]
    imports: RefCell<VecDeque<String>>,
    /// The people whose old conversation was taken in already.
    #[cfg(not(target_arch = "wasm32"))]
    imported: RefCell<BTreeSet<String>>,
    /// Emptying the old private store, once every conversation it held is in
    /// its pair's record.
    #[cfg(not(target_arch = "wasm32"))]
    clearing: RefCell<Option<crate::person_chat::Pending<()>>>,
    /// The old store is empty: the migration is done here.
    #[cfg(not(target_arch = "wasm32"))]
    cleared: Cell<bool>,
}

impl Records {
    pub fn new(handle: crate::framework::WorkspaceHandle, workspace: String, me: &str) -> Rc<Self> {
        Rc::new(Self {
            handle,
            workspace,
            root: me.to_owned(),
            me: crate::person_chat::person_key(me),
            people: RefCell::default(),
            open: RefCell::default(),
            #[cfg(not(target_arch = "wasm32"))]
            imports: RefCell::default(),
            #[cfg(not(target_arch = "wasm32"))]
            imported: RefCell::default(),
            #[cfg(not(target_arch = "wasm32"))]
            clearing: RefCell::default(),
            #[cfg(not(target_arch = "wasm32"))]
            cleared: Cell::default(),
        })
    }

    /// The people the window may talk to, and the conversation each of them
    /// already has: what the direct message list shows.
    pub fn set_people(&self, people: Vec<Person>) {
        #[cfg(not(target_arch = "wasm32"))]
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

    /// How many messages of `peer`'s conversation this person has not read.
    pub fn unread(&self, peer: &str) -> u32 {
        self.held(peer).map_or(0, |open| {
            open.record
                .get()
                .unread(&Self::conversation(&open), &self.me, peer)
        })
    }

    /// This person, under the key a conversation keeps them.
    pub fn person(&self) -> &str {
        &self.me
    }

    /// The pair id of the conversation with `root`.
    fn pair(&self, root: &str) -> Result<String, String> {
        crate::person_chat::direct_pair_id(&self.workspace, &self.root, root)
            .ok_or_else(|| "no direct conversation in this workspace".to_owned())
    }

    /// The person a conversation keeps under `peer`.
    fn person_of(&self, peer: &str) -> Option<Person> {
        self.people
            .borrow()
            .iter()
            .find(|person| person.key == peer)
            .cloned()
    }

    /// `pair`'s record and its parts, opened (the record created where the
    /// workspace holds none) and followed change by change.
    fn opened(&self, pair: &str) -> Option<Open> {
        let record = self
            .handle
            .record::<DirectConversation>(OWNER, &collab_types::direct_record(pair))?;
        let chain = self.handle.chain_beside::<DirectPart>(OWNER, &record)?;
        Some(Open { record, chain })
    }

    /// The conversation with `peer`, opened.
    fn record(&self, peer: &str) -> Result<Open, String> {
        let person = self
            .person_of(peer)
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
        let person = self.person_of(peer)?;
        let pair = self.pair(&person.root).ok()?;
        if let Some(open) = self.open.borrow().get(&pair) {
            return Some(open.clone());
        }
        if !self
            .handle
            .holds_record::<DirectConversation>(OWNER, &collab_types::direct_record(&pair))
        {
            return None;
        }
        let open = self.opened(&pair)?;
        self.open.borrow_mut().insert(pair, open.clone());
        Some(open)
    }

    /// The conversation with `peer` as a view shows it: the messages the
    /// record kept before the parts, and every message of the parts held
    /// here, oldest first.
    fn conversation(open: &Open) -> parts::Conversation {
        parts::Conversation::new(open.chain.parts())
    }

    /// The conversation with `peer` as a view shows it.
    fn shown(open: &Open, peer: &str) -> crate::person_chat::Conversation {
        let state = open.record.get();
        let parts = Self::conversation(open);
        let mut messages: BTreeMap<String, (i64, crate::person_chat::Message)> = state
            .messages
            .iter()
            .map(|(id, message)| {
                (
                    id.as_str().to_owned(),
                    (
                        message.at,
                        crate::person_chat::Message {
                            id: id.as_str().to_owned(),
                            author: message.author.clone(),
                            text: message.text.clone(),
                            at: message.at,
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
                    crate::person_chat::Message {
                        id: id.as_str().to_owned(),
                        author: message.author.clone(),
                        text: message.text.clone(),
                        at: message.at,
                    },
                ),
            );
        }
        let mut messages: Vec<(i64, crate::person_chat::Message)> =
            messages.into_values().collect();
        messages.sort_by_key(|(at, _)| *at);
        crate::person_chat::Conversation {
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

    /// Whether this person has read up to the newest message of `peer`'s
    /// conversation, marking them read when not: what an open conversation
    /// does as it shows what came in.
    pub fn mark_read(&self, peer: &str) {
        let Some(open) = self.held(peer) else {
            return;
        };
        let mark = Self::conversation(&open).read_to_end(peer);
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
    #[cfg(not(target_arch = "wasm32"))]
    fn import(&self, peer: &str) -> bool {
        // The door names people as the workspace does; what the record keeps
        // is under the key its conversations use.
        let Some(root) = self.person_of(peer).map(|person| person.root) else {
            return true;
        };
        let mut after = 0;
        let mut imported = Vec::new();
        loop {
            // The old store has nothing more here: done, whatever came.
            let Some(Ok(page)) = crate::person_chat::import_archive(&root, after).take() else {
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
        let Ok(open) = self.record(peer) else {
            return false;
        };
        // A part holds 2000 messages or 1 MiB of them, so a long
        // conversation crosses into as many parts as it fills, and the
        // part after it is started the same way a message that finds the
        // head full does.
        let mut rest = imported;
        while !rest.is_empty() {
            let conversation = Self::conversation(&open);
            let (capacity, text_capacity) = match conversation.parts().last() {
                Some((head, part)) if conversation.post_part() == *head => (
                    PART_MESSAGES.saturating_sub(part.messages.len()).max(1),
                    PART_TEXT_BYTES.saturating_sub(part.text_bytes()).max(1),
                ),
                _ => (PART_MESSAGES, PART_TEXT_BYTES),
            };
            let tail = imported_room(&mut rest, capacity, text_capacity);
            let chunk = std::mem::take(&mut rest);
            rest = tail;
            let taken = open
                .chain
                .invoke(&DirectPart::IMPORT.with(parts::direct_part::Import { messages: chunk }))
                .is_ok();
            if !taken {
                return false;
            }
        }
        self.imported.borrow_mut().insert(peer.to_owned());
        true
    }

    /// Import a few conversations the old private store still holds, so a
    /// person's history comes over once without stalling a frame; once none
    /// is left, empty the old store in one publication. A page holds no store
    /// of its own: the device that hosts the workspace imports it, and the
    /// records carry the conversations to every other device.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn import_old_store(&self) {
        for _ in 0..IMPORTS_PER_SYNC {
            let Some(peer) = self.imports.borrow_mut().pop_front() else {
                break;
            };
            if !self.import(&peer) {
                // Its record could not be written yet: ask again next sync.
                self.imports.borrow_mut().push_back(peer);
            }
        }
        if !self.imports.borrow().is_empty() || self.cleared.get() {
            return;
        }
        // A roster this window has not read yet proves no conversation was
        // imported, so nothing is emptied on its word.
        if self.people.borrow().is_empty() {
            return;
        }
        if self.clearing.borrow().is_some() {
            let answered = self
                .clearing
                .borrow()
                .as_ref()
                .and_then(crate::person_chat::Pending::take);
            match answered {
                Some(Ok(())) => {
                    self.cleared.set(true);
                    return;
                }
                // The workspace could not write it: ask again next sync.
                Some(Err(_)) => {
                    self.clearing.borrow_mut().take();
                }
                None => return,
            }
        }
        self.clearing
            .replace(Some(crate::person_chat::clear_archive()));
    }
}

/// Split off as many of `rest` as fit in a part with `room` messages and
/// `text_room` bytes left: at least one message, whatever it says.
#[cfg(not(target_arch = "wasm32"))]
fn imported_room(
    rest: &mut Vec<StoredMessage>,
    room: usize,
    text_room: usize,
) -> Vec<StoredMessage> {
    let mut take = 0;
    let mut bytes: usize = 0;
    while take < rest.len() && take < room {
        let Some(message) = rest.get(take) else {
            break;
        };
        let len = message.text.len();
        if take > 0 && bytes.saturating_add(len) > text_room {
            break;
        }
        bytes = bytes.saturating_add(len);
        take = take.saturating_add(1);
    }
    rest.split_off(take)
}

/// What Chat's views read and write: the same contacts and conversations
/// the person door used to answer, from the pair records this window holds.
impl Records {
    /// The workspace's people this person may talk to, each with whether a
    /// conversation of theirs holds messages.
    pub fn contacts(&self) -> Vec<crate::person_chat::Contact> {
        self.observe();
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
                crate::person_chat::Contact {
                    public_key: person.key.clone(),
                    display_name: person.name.clone(),
                    conversation,
                }
            })
            .collect()
    }

    /// Open the conversation records this device already holds, so a message
    /// that came in for one wakes this window like any other change of its
    /// own record: a conversation is named by the two people it is between,
    /// and one there is a record of here is read from now on.
    fn observe(&self) {
        let people = self.people.borrow().clone();
        for person in people {
            let Ok(pair) = self.pair(&person.root) else {
                continue;
            };
            if self.open.borrow().contains_key(&pair) {
                continue;
            }
            if !self
                .handle
                .holds_record::<DirectConversation>(OWNER, &collab_types::direct_record(&pair))
            {
                continue;
            }
            let opened = self.opened(&pair);
            if let Some(open) = opened {
                self.open.borrow_mut().insert(pair, open);
            }
        }
    }

    /// The whole conversation with `peer`, opened: its record is created
    /// where the workspace holds none yet.
    pub fn open(&self, peer: &str) -> Result<crate::person_chat::Conversation, String> {
        let open = self.record(peer)?;
        // The whole conversation is what a view opens: every earlier part is
        // asked for, and arrives change by change as the workspace sends it.
        for (index, _) in open.chain.parts() {
            let _ = open.chain.hold(index);
        }
        Ok(Self::shown(&open, peer))
    }

    /// The whole conversation with `peer`, as it stands.
    pub fn read(&self, peer: &str) -> Result<crate::person_chat::Conversation, String> {
        match self.held(peer) {
            Some(open) => Ok(Self::shown(&open, peer)),
            // No conversation here yet is an empty one, not a refusal.
            None if self.person_of(peer).is_some() => Ok(crate::person_chat::Conversation {
                peer: peer.to_owned(),
                messages: Vec::new(),
            }),
            None => Err("no direct conversation with them here".into()),
        }
    }

    /// Send `text` to `peer` once: `invocation` names the message, so a
    /// retry is the same message and not another.
    pub fn send(
        &self,
        peer: &str,
        text: &str,
        invocation: tilo_actions::InvocationId,
    ) -> Result<crate::person_chat::Conversation, String> {
        let open = self.record(peer)?;
        open.chain
            .invoke(&DirectPart::SEND.with(parts::direct_part::Send {
                id: parts::message_id(invocation),
                text: text.to_owned(),
            }))
            .map_err(|error| error.to_string())?;
        Ok(Self::shown(&open, peer))
    }
}
