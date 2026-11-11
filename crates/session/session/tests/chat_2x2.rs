//! Chat's direct conversations between two people in one workspace, each on two real devices: the
//! app's acceptance (`crates/sdk/tilo-app/src/direct/acceptance.rs`) driven through the sessions'
//! doors.
//! Each device is a `tilo-session` of its own on one workspace a site hosts ([`common::people`]),
//! with the Chat package this run built installed in its store: opening Chat starts the app, and
//! every step below reads what the app shows through the actions it publishes to its session — the
//! conversation's messages, its unread count, a send's answer — never the records or the broker's
//! state behind them.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the devices live \
              until their sessions end"
)]

mod common;

// A site admits its members' records through an Automerge worker: this binary is that worker, as
// the session's own is.
tilo_automerge_isolate::test_worker!();

use common::people::{Device, Person, Site};
use tilo_relay_protocol::workspaces::WorkspaceRole;

/// A studio and the two people in it, each on a laptop and a phone: Ada's site hosts the workspace
/// and lets Chat author its records, Ada and Bob join as editors, and Chat is installed and open on
/// all four.
/// Dropped in this order: the devices stop while the site still hosts them.
struct Studio {
    ada_laptop: Device,
    ada_phone: Device,
    bob_laptop: Device,
    bob_phone: Device,
    ada: Person,
    bob: Person,
    site: Site,
}

impl Studio {
    fn start() -> Self {
        let ada = Person::new("Ada");
        let bob = Person::new("Bob");
        let site = Site::start(&ada, "Studio");
        site.publish(WorkspaceRole::Editor);
        // The workspace's host lets Chat author its records, as the host of a workspace does before
        // its people open apps there: Chat writes the people who introduced themselves into its own
        // record, and a member's device cannot grant that for itself.
        site.author_apps(&["so.tilo.chat"]);
        let mut ada_laptop = ada.device("Ada's laptop");
        let mut ada_phone = ada.device("Ada's phone");
        for device in [&mut ada_laptop, &mut ada_phone] {
            let member = site.admit(ada.public);
            device.install_chat();
            device.join(&member);
            device.wait_holding(&site.workspace);
            device.wait_showing(&site.workspace);
        }
        ada_laptop.open("so.tilo.chat");
        ada_laptop.wait_for_window("so.tilo.chat");
        let mut bob_laptop = bob.device("Bob's laptop");
        let mut bob_phone = bob.device("Bob's phone");
        for device in [&mut bob_laptop, &mut bob_phone] {
            let member = site.admit(bob.public);
            device.install_chat();
            device.join(&member);
            device.wait_holding(&site.workspace);
            device.wait_showing(&site.workspace);
            device.open("so.tilo.chat");
            device.wait_for_window("so.tilo.chat");
        }
        ada_phone.open("so.tilo.chat");
        ada_phone.wait_for_window("so.tilo.chat");
        // What a window writes reaches the workspace's host: the record Chat keeps its workspace in
        // is one the site holds, and the people it introduces are read from there on every device.
        site.wait_for_record_of("so.tilo.chat", &ada_laptop);
        // Each person's Chat shows the other once their windows have said who they are in the
        // workspace's shared record: what a conversation needs before it can open.
        let ada_key = hex::encode(ada.public);
        let bob_key = hex::encode(bob.public);
        for device in [&bob_laptop, &bob_phone] {
            device.chat().wait_for_contact(&ada_key);
        }
        for device in [&ada_laptop, &ada_phone] {
            device.chat().wait_for_contact(&bob_key);
        }
        Self {
            site,
            ada,
            bob,
            ada_laptop,
            ada_phone,
            bob_laptop,
            bob_phone,
        }
    }

    /// The two people's root keys, which the workspace names them by.
    fn keys(&self) -> (String, String) {
        (hex::encode(self.ada.public), hex::encode(self.bob.public))
    }
}

#[test]
fn a_message_reaches_both_people_on_both_of_their_devices_live() {
    let studio = Studio::start();
    let (ada_key, bob_key) = studio.keys();
    // Bob's laptop has the conversation open while Ada writes: what he sees is the message arriving
    // in the conversation, not a page reloaded.
    studio.bob_laptop.chat().open(&ada_key);
    studio.ada_laptop.chat().send(&bob_key, "hello Bob");
    for (device, peer) in [
        (&studio.ada_laptop, &bob_key),
        (&studio.ada_phone, &bob_key),
        (&studio.bob_laptop, &ada_key),
        (&studio.bob_phone, &ada_key),
    ] {
        device.chat().wait_for_texts(peer, &["hello Bob"]);
    }
    // The same message, under the same id, on every device.
    let ids: Vec<Vec<String>> = [&studio.ada_laptop, &studio.ada_phone]
        .iter()
        .map(|device| message_ids(device, &bob_key))
        .collect();
    assert_eq!(ids[0].len(), 1, "one message: {ids:?}");
    assert_eq!(
        ids[1], ids[0],
        "Ada's phone reads the message her laptop sent"
    );
    for device in [&studio.bob_laptop, &studio.bob_phone] {
        assert_eq!(
            message_ids(device, &ada_key),
            ids[0],
            "Bob reads the same message"
        );
    }
}

/// Two desktops talk to each other: what one writes arrives in the other's open conversation, and
/// its answer comes back the same way.
#[test]
fn two_desktops_message_each_other() {
    let studio = Studio::start();
    let (ada_key, bob_key) = studio.keys();
    studio.ada_laptop.chat().open(&bob_key);
    studio.bob_laptop.chat().open(&ada_key);
    studio.ada_laptop.chat().send(&bob_key, "hello Bob");
    studio
        .bob_laptop
        .chat()
        .wait_for_texts(&ada_key, &["hello Bob"]);
    studio.bob_laptop.chat().send(&ada_key, "hello Ada");
    studio
        .ada_laptop
        .chat()
        .wait_for_texts(&bob_key, &["hello Bob", "hello Ada"]);
    assert_eq!(
        message_ids(&studio.ada_laptop, &bob_key),
        message_ids(&studio.bob_laptop, &ada_key),
        "both desktops read the conversation as one"
    );
}

/// The ids of the messages the conversation with `peer` shows, in the app's order.
fn message_ids(device: &Device, peer: &str) -> Vec<String> {
    device
        .chat()
        .conversation(peer)
        .messages
        .iter()
        .filter_map(|message| message["id"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn messages_the_host_kept_reach_a_device_that_was_away() {
    let mut studio = Studio::start();
    let (ada_key, bob_key) = studio.keys();
    // Bob's devices are off while Ada writes: nothing of hers reaches them yet, and the site keeps
    // what she wrote.
    studio.bob_laptop.stop();
    studio.bob_phone.stop();
    for text in ["one", "two", "three"] {
        studio.ada_laptop.chat().send(&bob_key, text);
    }
    // Bob's laptop starts again: the workspace hands it all three, in order,
    // and his phone the same when it comes back.
    studio.bob_laptop.start();
    studio.bob_laptop.wait_holding(&studio.site.workspace);
    studio.bob_laptop.wait_showing(&studio.site.workspace);
    studio.bob_laptop.open("so.tilo.chat");
    studio.bob_laptop.wait_for_window("so.tilo.chat");
    studio
        .bob_laptop
        .chat()
        .wait_for_texts(&ada_key, &["one", "two", "three"]);
    studio.bob_phone.start();
    studio.bob_phone.wait_holding(&studio.site.workspace);
    studio.bob_phone.wait_showing(&studio.site.workspace);
    studio.bob_phone.open("so.tilo.chat");
    studio.bob_phone.wait_for_window("so.tilo.chat");
    studio
        .bob_phone
        .chat()
        .wait_for_texts(&ada_key, &["one", "two", "three"]);
    // Bob answers, and Ada's laptop—which stayed on—shows it with her own three.
    studio.bob_phone.chat().send(&ada_key, "answered");
    studio
        .ada_laptop
        .chat()
        .wait_for_texts(&bob_key, &["one", "two", "three", "answered"]);
    // Ada's phone was away too: it reads the whole conversation at once, under the same ids.
    studio
        .ada_phone
        .chat()
        .wait_for_texts(&bob_key, &["one", "two", "three", "answered"]);
    // Every device reads that conversation off the copy of its record it keeps itself: the
    // workspace's host gone, all of them still show it, under the same ids.
    studio.site.stop();
    let ids = message_ids(&studio.ada_laptop, &bob_key);
    for (device, peer) in [
        (&studio.ada_phone, &bob_key),
        (&studio.bob_laptop, &ada_key),
        (&studio.bob_phone, &ada_key),
    ] {
        assert_eq!(
            message_ids(device, peer),
            ids,
            "{} reads its own copy of the conversation",
            device.label
        );
    }
}

#[test]
fn both_people_writing_offline_converge_without_duplicates() {
    let mut studio = Studio::start();
    let (ada_key, bob_key) = studio.keys();
    studio.ada_laptop.chat().send(&bob_key, "Ada one");
    studio
        .bob_laptop
        .chat()
        .wait_for_texts(&ada_key, &["Ada one"]);
    // Each person writes while the other's other device is away: neither pair of devices has heard
    // the other's message yet.
    studio.ada_phone.stop();
    studio.bob_phone.stop();
    studio.ada_laptop.chat().send(&bob_key, "Ada two");
    studio.bob_laptop.chat().send(&ada_key, "Bob one");
    studio.bob_laptop.chat().send(&ada_key, "Bob two");
    for device in [&mut studio.ada_phone, &mut studio.bob_phone] {
        device.start();
        device.wait_holding(&studio.site.workspace);
        device.wait_showing(&studio.site.workspace);
        device.open("so.tilo.chat");
        device.wait_for_window("so.tilo.chat");
    }
    // Every message reaches every device. The two written offline at the same time have no order of
    // their own until the records give them one: each device waits for all of them, whichever order
    // it reads them in.
    let wanted = ["Ada one", "Ada two", "Bob one", "Bob two"];
    for (device, peer) in [
        (&studio.ada_laptop, &bob_key),
        (&studio.ada_phone, &bob_key),
        (&studio.bob_laptop, &ada_key),
        (&studio.bob_phone, &ada_key),
    ] {
        device.chat().wait_for_texts_in_any_order(peer, &wanted);
    }
    // Each message once, under one id, on every device, in one order: the same one every device
    // reads.
    let ids = message_ids(&studio.ada_laptop, &bob_key);
    assert_eq!(ids.len(), 4, "each message once: {ids:?}");
    let unique: std::collections::BTreeSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), 4, "no id twice: {ids:?}");
    for (device, peer) in [
        (&studio.ada_phone, &bob_key),
        (&studio.bob_laptop, &ada_key),
        (&studio.bob_phone, &ada_key),
    ] {
        assert_eq!(
            message_ids(device, peer),
            ids,
            "{} reads the same messages",
            device.label
        );
    }
}

#[test]
fn one_read_mark_advances_once_and_both_of_a_persons_devices_agree() {
    let studio = Studio::start();
    let (ada_key, bob_key) = studio.keys();
    for text in ["one", "two", "three"] {
        studio.ada_laptop.chat().send(&bob_key, text);
    }
    // Bob has read none of Ada's on either device.
    for device in [&studio.bob_laptop, &studio.bob_phone] {
        device.chat().wait_for_unread(&ada_key, 3);
    }
    // Reading them on his laptop marks them read for his phone too.
    studio.bob_laptop.chat().open(&ada_key);
    studio.bob_laptop.chat().wait_for_unread(&ada_key, 0);
    studio.bob_phone.chat().wait_for_unread(&ada_key, 0);
    // Read is not gone: both of his devices still show what Ada wrote.
    for device in [&studio.bob_laptop, &studio.bob_phone] {
        device
            .chat()
            .wait_for_texts(&ada_key, &["one", "two", "three"]);
    }
    // Ada's own messages are hers: nothing of Bob's is unread for her, and nothing of hers is
    // unread for herself.
    for device in [&studio.ada_laptop, &studio.ada_phone] {
        assert_eq!(device.chat().unread(&bob_key), 0);
    }
}

#[test]
fn another_pair_is_another_conversation() {
    let studio = Studio::start();
    let (ada_key, bob_key) = studio.keys();
    studio.ada_laptop.chat().send(&bob_key, "just us");
    studio
        .bob_laptop
        .chat()
        .wait_for_texts(&ada_key, &["just us"]);
    // Carol joins the same studio, and Chat offers her Bob as someone to talk to: what she reads
    // there is her own conversation with him, not his with Ada.
    let carol = Person::new("Carol");
    let mut carol_laptop = carol.device("Carol's laptop");
    let member = studio.site.admit(carol.public);
    carol_laptop.install_chat();
    carol_laptop.join(&member);
    carol_laptop.wait_holding(&studio.site.workspace);
    carol_laptop.wait_showing(&studio.site.workspace);
    carol_laptop.open("so.tilo.chat");
    carol_laptop.wait_for_window("so.tilo.chat");
    carol_laptop.chat().wait_for_contact(&bob_key);
    assert!(
        carol_laptop.chat().texts(&bob_key).is_empty(),
        "Carol reads nothing of the conversation between Ada and Bob"
    );
    // And a person the workspace never admitted is someone Chat refuses to open a conversation
    // with: what Carol reads with Bob is the pair's own record, and a stranger is in no pair with
    // her.
    let stranger = hex::encode(Person::new("Stranger").public);
    assert!(
        carol_laptop.chat().refusal(&stranger).is_some(),
        "Chat refuses a conversation with someone in no workspace with her"
    );
    // And Bob's conversation with Ada shows nothing of Carol.
    assert_eq!(
        studio.bob_laptop.chat().texts(&ada_key),
        vec!["just us".to_owned()],
        "Bob's conversation with Ada is theirs"
    );
}
