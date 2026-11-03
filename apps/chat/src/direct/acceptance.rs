//! Two people's direct conversation, on two devices each, through the
//! workspace's store: what each of them reads and how far, over the pair's
//! record and its parts.
//!
//! The four devices keep their own copies of the pair's records and hand the
//! workspace's store their bytes, as a device syncs its records through the
//! host: the store holds what it is given and nothing more (no key, no
//! readable conversation), so A→B works when the two people are never online
//! at the same time. The store can be restarted between deliveries without
//! losing what it kept. What a stranger reads of a pair's records, and what
//! the host may decrypt, is the workspace's rule
//! (`tilo_peer_state::state::direct_pairs_tests`); this test proves the
//! conversation those rules carry.

use std::collections::{BTreeMap, BTreeSet};

use ui::tilo_resource::Replica;

use super::*;
use crate::direct_parts::{direct_part, post_part};

/// Ada's root public key, as the workspace and the conversation name her.
const ADA: &str = "adadadadadadadadadadadadadadadadadadadadadadadadadadadadadadadad";
/// Bob's.
const BOB: &str = "b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0";

/// The workspace's store of the pair's records, as it carries them between
/// devices: the bytes it is handed, and nothing it could read them with.
#[derive(Default)]
struct Store {
    parts: BTreeMap<u64, Vec<u8>>,
    record: Option<Vec<u8>>,
}

impl Store {
    /// Keep `part` beside what the store already holds of the same part: a
    /// store merges what devices hand it, never replacing another device's
    /// messages with its own copy.
    fn keep_part(&mut self, index: u64, part: &mut Replica<DirectPart>) {
        match self.parts.get(&index) {
            Some(bytes) => {
                let mut kept = Replica::<DirectPart>::load(bytes).expect("a kept part loads");
                kept.sync_with(part).expect("a part merges");
                self.parts.insert(index, kept.save());
            }
            None => {
                self.parts.insert(index, part.save());
            }
        }
    }

    /// [`Self::keep_part`] for the pair's record and its read marks.
    fn keep_record(&mut self, record: &mut Replica<DirectConversation>) {
        match &self.record {
            Some(bytes) => {
                let mut kept =
                    Replica::<DirectConversation>::load(bytes).expect("a kept record loads");
                kept.sync_with(record).expect("a record merges");
                self.record = Some(kept.save());
            }
            None => self.record = Some(record.save()),
        }
    }

    /// The same store after the process that held it restarted: what it kept
    /// is still there.
    fn restarted(&self) -> Self {
        Self {
            parts: self.parts.clone(),
            record: self.record.clone(),
        }
    }
}

/// One device of one of the two people: its copies of the conversation's
/// record and parts, and its clock.
struct Device {
    /// The person whose device this is, as the conversation names them.
    person: &'static str,
    parts: BTreeMap<u64, Replica<DirectPart>>,
    /// The pair's record and its read marks, opened from the store the first
    /// time this device needs it: a device writes a record that exists, and
    /// the first one to need it creates the one every device then holds.
    marks: Option<Replica<DirectConversation>>,
    /// The device's clock, advancing with every message it writes.
    clock: i64,
}

impl Device {
    fn new(person: &'static str, clock: i64) -> Self {
        Self {
            person,
            parts: BTreeMap::new(),
            marks: None,
            clock,
        }
    }

    /// The pair's record as this device holds it, taken from the store where
    /// the workspace holds one already, created here and given to the store
    /// where it holds none.
    fn marks(&mut self, store: &mut Store) -> &mut Replica<DirectConversation> {
        if self.marks.is_none() {
            let mut record = match &store.record {
                Some(bytes) => {
                    Replica::<DirectConversation>::load(bytes).expect("the kept record loads")
                }
                None => Replica::<DirectConversation>::new(),
            };
            record = record.as_invoker(self.person);
            record.set_now_ms(self.clock);
            if store.record.is_none() {
                store.keep_record(&mut record);
            }
            self.marks = Some(record);
        }
        self.marks.as_mut().expect("the record is open")
    }

    /// Say `text` as `id`: the message goes to the head, or to the part after
    /// it once the head is full, carrying the counts before it. A device
    /// writes a record it holds: it takes in what the store has of it
    /// first, as it synced before the person wrote.
    fn send(&mut self, store: &Store, id: &str, text: &str) {
        self.download(store);
        self.clock += 1;
        let head = self.parts.last_key_value().map(|(index, part)| {
            let state = part.state().expect("a part reads");
            (
                *index,
                post_part(*index, state.messages.len(), state.text_bytes()),
            )
        });
        let index = head.map_or(0, |(_, next)| next);
        let mut part = self.parts.remove(&index).unwrap_or_else(|| {
            let mut part = Replica::<DirectPart>::new();
            if let Some(previous) = self.parts.get(&index.saturating_sub(1))
                && index > 0
            {
                let carried = previous.state().expect("the part before reads");
                part = part.as_invoker(self.person);
                part.apply(DirectPart::CARRY_COUNTS.with(direct_part::CarryCounts {
                    earlier: carried.carried_after(index.saturating_sub(1)),
                }))
                .expect("a part carries the counts before it");
            }
            part
        });
        part = part.as_invoker(self.person);
        part.set_now_ms(self.clock);
        part.apply(DirectPart::SEND.with(direct_part::Send {
            id: id.to_owned(),
            text: text.to_owned(),
        }))
        .expect("a pair's person says something");
        self.parts.insert(index, part);
    }

    /// Take in messages the old private store kept, as the import does: into
    /// a part the device holds, after taking in what the store has of it.
    fn import(&mut self, store: &Store, messages: Vec<crate::direct_parts::StoredMessage>) {
        self.download(store);
        let index = self.parts.keys().next_back().copied().unwrap_or(0);
        let mut part = self.parts.remove(&index).unwrap_or_default();
        part = part.as_invoker(self.person);
        part.apply(DirectPart::IMPORT.with(direct_part::Import { messages }))
            .expect("a pair's person imports their conversation");
        self.parts.insert(index, part);
    }

    /// Mark how far this person has read, as the conversation's open view
    /// does, on the record the device holds.
    fn mark_read(&mut self, store: &mut Store, part: i64, seen: i64) {
        self.download(store);
        self.clock += 1;
        let clock = self.clock;
        let record = self.marks(store);
        record.set_now_ms(clock);
        record
            .apply(DirectConversation::MARK_READ.with(conversation::MarkRead { part, seen }))
            .expect("a person marks what they read");
    }

    /// The conversation as this device holds it.
    fn conversation(&self) -> direct_parts::Conversation {
        let parts: Vec<(u64, std::rc::Rc<DirectPart>)> = self
            .parts
            .iter()
            .map(|(index, part)| {
                (
                    *index,
                    std::rc::Rc::new(part.state().expect("a part reads")),
                )
            })
            .collect();
        direct_parts::Conversation::new(parts)
    }

    /// What this device shows of the conversation: each message's id and
    /// what it says, oldest first.
    fn shown(&self) -> Vec<(String, String)> {
        self.conversation()
            .messages()
            .map(|(id, message)| (id.as_str().to_owned(), message.text.clone()))
            .collect()
    }

    /// How many of `peer`'s messages this person has not read.
    fn unread(&mut self, store: &mut Store, peer: &str) -> u32 {
        self.download(store);
        let state = self.marks(store).state().expect("a record reads");
        state.unread(
            &self.conversation(),
            &ui::person_chat::person_key(self.person),
            &ui::person_chat::person_key(peer),
        )
    }

    /// Hand the store this device's copy of every part, and of the record.
    fn upload(&mut self, store: &mut Store) {
        for (index, part) in &mut self.parts {
            store.keep_part(*index, part);
        }
        if let Some(record) = &mut self.marks {
            store.keep_record(record);
        }
    }

    /// Take in what the store holds, as syncing through the host does.
    fn download(&mut self, store: &Store) {
        for (index, bytes) in &store.parts {
            let mut incoming = Replica::<DirectPart>::load(bytes).expect("a kept part loads");
            match self.parts.get_mut(index) {
                Some(part) => part.sync_with(&mut incoming).expect("a part merges"),
                None => {
                    self.parts.insert(*index, incoming);
                }
            }
        }
        if let Some(bytes) = &store.record
            && let Some(record) = &mut self.marks
        {
            let mut incoming =
                Replica::<DirectConversation>::load(bytes).expect("a kept record loads");
            record.sync_with(&mut incoming).expect("a record merges");
        }
    }

    /// `peer`'s devices and this one exchange their records directly: what
    /// two devices that meet do, without the host in between.
    fn meet(&mut self, peer: &mut Self) {
        for (index, part) in &mut self.parts {
            if let Some(theirs) = peer.parts.get_mut(index) {
                part.sync_with(theirs).expect("a part merges");
            }
        }
        for (index, part) in &mut peer.parts {
            if !self.parts.contains_key(index) {
                self.parts.insert(
                    *index,
                    Replica::<DirectPart>::load(&part.save()).expect("a part loads"),
                );
            }
        }
        match (&mut self.marks, &mut peer.marks) {
            (Some(mine), Some(theirs)) => mine.sync_with(theirs).expect("a record merges"),
            (Some(mine), None) => {
                peer.marks = Some(Replica::load(&mine.save()).expect("a record loads"))
            }
            (None, Some(theirs)) => {
                self.marks = Some(Replica::load(&theirs.save()).expect("a record loads"))
            }
            (None, None) => {}
        }
    }
}

/// The four devices of the two people.
fn devices() -> (Device, Device, Device, Device) {
    (
        Device::new(ADA, 1_000),
        Device::new(ADA, 2_000),
        Device::new(BOB, 3_000),
        Device::new(BOB, 4_000),
    )
}

#[test]
fn a_message_reaches_both_people_on_both_of_their_devices_live() {
    let (mut ada_laptop, mut ada_phone, mut bob_laptop, mut bob_phone) = devices();
    let mut store = Store::default();
    ada_laptop.send(&store, "dm_hello", "hello Bob");
    ada_laptop.upload(&mut store);
    for device in [&mut ada_phone, &mut bob_laptop, &mut bob_phone] {
        device.download(&store);
    }
    for device in [&ada_laptop, &ada_phone, &bob_laptop, &bob_phone] {
        assert_eq!(
            device.shown(),
            [("dm_hello".to_owned(), "hello Bob".to_owned())],
            "each device reads the one message, under the same id"
        );
    }
    // Ada's other device already held it: the second download adds nothing.
    ada_phone.download(&store);
    ada_phone.download(&store);
    assert_eq!(ada_phone.shown().len(), 1, "the same message is read once");
}

#[test]
fn three_messages_kept_by_the_host_reach_a_device_that_was_away() {
    let (mut ada_laptop, mut ada_phone, mut bob_laptop, mut bob_phone) = devices();
    let mut store = Store::default();
    // Bob's devices are off: nothing of Ada's reaches them yet.
    for (id, text) in [("dm_1", "one"), ("dm_2", "two"), ("dm_3", "three")] {
        ada_laptop.send(&store, id, text);
        ada_laptop.upload(&mut store);
    }
    assert!(bob_laptop.shown().is_empty(), "Bob has seen nothing");
    // Bob's laptop starts and the host hands it all three, in order.
    bob_laptop.download(&store);
    assert_eq!(
        bob_laptop
            .shown()
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>(),
        ["one", "two", "three"]
    );
    // The host restarts; what it kept is still there.
    let mut store = store.restarted();
    bob_phone.download(&store);
    assert_eq!(
        bob_phone.shown().len(),
        3,
        "the second device gets all three"
    );
    // Bob answers while Ada's laptop stays online: her device reads it.
    bob_phone.send(&store, "dm_answer", "answered");
    bob_phone.upload(&mut store);
    ada_laptop.download(&store);
    ada_phone.download(&store);
    assert_eq!(ada_laptop.shown().len(), 4);
    assert_eq!(ada_phone.shown(), ada_laptop.shown());
    assert_eq!(
        bob_laptop.shown().len(),
        3,
        "Bob's laptop has not synced yet"
    );
    bob_laptop.download(&store);
    assert_eq!(bob_laptop.shown(), bob_phone.shown());
}

#[test]
fn both_people_writing_offline_converge_without_duplicates() {
    let (mut ada_laptop, mut ada_phone, mut bob_laptop, mut bob_phone) = devices();
    let mut store = Store::default();
    ada_laptop.send(&store, "dm_a1", "Ada one");
    ada_laptop.upload(&mut store);
    bob_laptop.download(&store);
    // Both write offline: neither sees the other's message yet.
    ada_laptop.send(&store, "dm_a2", "Ada two");
    bob_laptop.send(&store, "dm_b1", "Bob one");
    bob_laptop.send(&store, "dm_b2", "Bob two");
    ada_laptop.upload(&mut store);
    bob_laptop.upload(&mut store);
    for device in [
        &mut ada_laptop,
        &mut ada_phone,
        &mut bob_laptop,
        &mut bob_phone,
    ] {
        device.download(&store);
        device.download(&store);
    }
    let shown = ada_laptop.shown();
    let ids: BTreeSet<&str> = shown.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(shown.len(), 4, "each message once: {shown:?}");
    assert_eq!(ids.len(), 4, "no id twice: {shown:?}");
    assert_eq!(ada_phone.shown(), shown);
    assert_eq!(
        bob_laptop.shown(),
        shown,
        "both people read the same conversation"
    );
    assert_eq!(bob_phone.shown(), shown);
    // The same send asked for twice is the same message, not another.
    ada_laptop.send(&store, "dm_a2", "Ada two");
    ada_laptop.upload(&mut store);
    bob_phone.download(&store);
    assert_eq!(bob_phone.shown().len(), 4);
}

#[test]
fn one_read_mark_advances_once_and_both_of_a_persons_devices_agree() {
    let (mut ada_laptop, mut ada_phone, mut bob_laptop, mut bob_phone) = devices();
    let mut store = Store::default();
    for (id, text) in [("dm_1", "one"), ("dm_2", "two"), ("dm_3", "three")] {
        ada_laptop.send(&store, id, text);
    }
    ada_laptop.upload(&mut store);
    // Bob's laptop is online: it holds the conversation's record before he
    // writes in it.
    bob_laptop.download(&store);
    bob_laptop.send(&store, "dm_b1", "Bob's");
    bob_laptop.upload(&mut store);
    for device in [
        &mut ada_laptop,
        &mut ada_phone,
        &mut bob_laptop,
        &mut bob_phone,
    ] {
        device.download(&store);
    }
    assert_eq!(
        bob_laptop.unread(&mut store, ADA),
        3,
        "Bob has read none of Ada's"
    );
    assert_eq!(
        ada_laptop.unread(&mut store, BOB),
        1,
        "Ada has read none of Bob's"
    );
    // Bob reads all three of Ada's in part 0.
    bob_laptop.mark_read(&mut store, 0, 3);
    bob_laptop.upload(&mut store);
    bob_phone.download(&store);
    ada_laptop.download(&store);
    assert_eq!(bob_laptop.unread(&mut store, ADA), 0);
    assert_eq!(bob_phone.unread(&mut store, ADA), 0, "Bob's devices agree");
    // Ada's devices agree too: her mark is her own, so Bob's message is
    // still unread on both.
    assert_eq!(ada_laptop.unread(&mut store, BOB), 1);
    assert_eq!(ada_phone.unread(&mut store, BOB), 1);
    // A further mark that says the same thing moves nothing, and one more
    // message from Ada is one more unread for Bob.
    bob_laptop.mark_read(&mut store, 0, 3);
    ada_laptop.send(&store, "dm_4", "four");
    ada_laptop.upload(&mut store);
    bob_phone.download(&store);
    bob_laptop.download(&store);
    bob_laptop.upload(&mut store);
    bob_phone.download(&store);
    assert_eq!(bob_laptop.unread(&mut store, ADA), 1);
    assert_eq!(bob_phone.unread(&mut store, ADA), 1);
}

#[test]
fn a_conversation_kept_in_the_old_store_imports_readable_once() {
    let (mut ada_laptop, _ada_phone, mut bob_laptop, mut bob_phone) = devices();
    let mut store = Store::default();
    // What the old private store holds for Bob: Ada's two messages and one
    // of his, written before the conversation moved to the pair's record.
    let old = |id: &str, author: &str, text: &str, at: i64| crate::direct_parts::StoredMessage {
        id: id.to_owned(),
        author: ui::person_chat::person_key(author),
        text: text.to_owned(),
        at,
    };
    bob_laptop.import(
        &store,
        vec![
            old("dm_old1", ADA, "before the records", 10),
            old("dm_old2", BOB, "and mine", 11),
            old("dm_old3", ADA, "and one more", 12),
        ],
    );
    bob_laptop.upload(&mut store);
    for device in [&mut ada_laptop, &mut bob_phone] {
        device.download(&store);
    }
    assert_eq!(
        ada_laptop
            .shown()
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>(),
        ["before the records", "and mine", "and one more"]
    );
    assert_eq!(bob_phone.shown(), ada_laptop.shown());
    // Bob imports the same conversation again: nothing is said twice.
    bob_phone.import(
        &store,
        vec![
            old("dm_old1", ADA, "before the records", 10),
            old("dm_old2", BOB, "and mine", 11),
        ],
    );
    assert_eq!(bob_phone.shown().len(), 3);
    assert_eq!(
        bob_phone.unread(&mut store, ADA),
        2,
        "Ada's two are unread for Bob"
    );
    assert_eq!(
        ada_laptop.unread(&mut store, BOB),
        1,
        "Bob's one is unread for Ada"
    );
}

#[test]
fn a_conversation_that_fills_a_part_keeps_going_in_the_next_one() {
    let mut ada = Device::new(ADA, 1_000);
    let mut store = Store::default();
    for index in 0..=PART_MESSAGES {
        ada.send(&store, &format!("dm_{index}"), "a message");
    }
    assert_eq!(
        ada.parts.keys().copied().collect::<Vec<_>>(),
        [0, 1],
        "the part fills at {PART_MESSAGES} messages and the next one starts"
    );
    ada.upload(&mut store);
    let mut bob = Device::new(BOB, 2_000);
    bob.download(&store);
    assert_eq!(
        bob.shown().len(),
        PART_MESSAGES + 1,
        "all of the conversation is there, across both parts"
    );
    assert_eq!(
        bob.unread(&mut store, ADA),
        2001,
        "the count reaches back through the parts"
    );
}

/// The pair id of the conversation, as the workspace derives it: the record
/// a stranger cannot read is another pair's, not this one's.
#[test]
fn another_pair_is_another_record() {
    let workspace = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
    let carol = "c0".repeat(32);
    let ours = collab_types::direct_pair_id(workspace, ADA, BOB).expect("a pair");
    let theirs = collab_types::direct_pair_id(workspace, ADA, &carol).expect("a pair");
    assert_ne!(ours, theirs);
    assert_ne!(
        ui::record::workspace::direct_record_id(&ours),
        ui::record::workspace::direct_record_id(&theirs)
    );
}
