//! Chat's direct conversations between two people in one workspace, each on two real devices.
//! Each device is a `tilo-session` of its own on one workspace a site hosts ([`common::people`]),
//! serving the development session's tilolang apps (Chat among them) as a development session does:
//! every step below goes through the actions Chat publishes to the sessions (`write_to`,
//! `conversation_with`, `unread_count`, `message_person`) and reads what the sessions list (the
//! conversation's messages, the notices a person is shown, the windows), never the records or the
//! broker's state behind them.

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

use crate::common::people::{
    Device, Person, Site, at, join_together, start_together, target,
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tilo_actions::remote::RemoteActionTarget;
use tilo_server_protocol::workspaces::Role;

/// Chat's package id: what its windows, its actions and its notices are listed under.
const CHAT: &str = "so.tilo.chat";
/// The kind a Chat message is published under.
const MESSAGE: &str = "tl.so.tilo.chat.Message";
/// The kind a direct conversation is published under.
const DIRECT: &str = "tl.so.tilo.chat.Direct";

/// A studio and the two people in it, each on a laptop and a phone: Ada's site hosts the workspace,
/// Ada and Bob join as editors, and Chat is served on all four.
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
        site.publish(Role::Editor);
        let mut ada_laptop = ada.device("Ada's laptop");
        let mut ada_phone = ada.device("Ada's phone");
        let mut bob_laptop = bob.device("Bob's laptop");
        let mut bob_phone = bob.device("Bob's phone");
        for device in [&ada_laptop, &ada_phone, &bob_laptop, &bob_phone] {
            device.install_dev_apps();
        }
        // The four devices join the workspace its entry admits them to, and none needs another
        // started first: every session boots before any is waited for.
        let hers = site.admit(ada.public);
        let her_phones = site.admit(ada.public);
        let his = site.admit(bob.public);
        let his_phones = site.admit(bob.public);
        join_together(
            &mut [
                &mut ada_laptop,
                &mut ada_phone,
                &mut bob_laptop,
                &mut bob_phone,
            ],
            &[
                hers.as_str(),
                her_phones.as_str(),
                his.as_str(),
                his_phones.as_str(),
            ],
        );
        for device in [&ada_laptop, &ada_phone, &bob_laptop, &bob_phone] {
            show_chat(device, &site);
        }
        // Each person's Chat can write to the other once the workspace names both as its members:
        // what a conversation needs before it can open.
        let ada_key = hex::encode(ada.public);
        let bob_key = hex::encode(bob.public);
        for device in [&bob_laptop, &bob_phone] {
            wait_for_member(device, &ada_key);
        }
        for device in [&ada_laptop, &ada_phone] {
            wait_for_member(device, &bob_key);
        }
        Self {
            ada_laptop,
            ada_phone,
            bob_laptop,
            bob_phone,
            ada,
            bob,
            site,
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
    send(&studio.ada_laptop, &bob_key, "hello Bob");
    for (device, peer) in [
        (&studio.ada_laptop, &bob_key),
        (&studio.ada_phone, &bob_key),
        (&studio.bob_laptop, &ada_key),
        (&studio.bob_phone, &ada_key),
    ] {
        wait_for_texts(device, peer, &["hello Bob"]);
    }
    // The same message, in the same conversation, on every device.
    let ids = message_ids(&studio.ada_laptop, &bob_key);
    assert_eq!(ids.len(), 1, "one message: {ids:?}");
    assert_eq!(
        message_ids(&studio.ada_phone, &bob_key),
        ids,
        "Ada's phone reads the message her laptop sent"
    );
    for device in [&studio.bob_laptop, &studio.bob_phone] {
        assert_eq!(
            message_ids(device, &ada_key),
            ids,
            "Bob reads the same message"
        );
    }
    let conversation = direct(&studio.ada_laptop, &bob_key);
    for (device, peer) in [
        (&studio.ada_phone, &bob_key),
        (&studio.bob_laptop, &ada_key),
        (&studio.bob_phone, &ada_key),
    ] {
        assert_eq!(
            direct(device, peer),
            conversation,
            "{} reads the one conversation between Ada and Bob",
            device.label
        );
    }
}

/// Two desktops talk to each other: what one writes arrives in the other's conversation, and its
/// answer comes back the same way, in the one conversation the two of them have.
#[test]
fn two_desktops_message_each_other() {
    let studio = Studio::start();
    let (ada_key, bob_key) = studio.keys();
    send(&studio.ada_laptop, &bob_key, "hello Bob");
    wait_for_texts(&studio.bob_laptop, &ada_key, &["hello Bob"]);
    send(&studio.bob_laptop, &ada_key, "hello Ada");
    wait_for_texts(&studio.ada_laptop, &bob_key, &["hello Bob", "hello Ada"]);
    assert_eq!(
        message_ids(&studio.ada_laptop, &bob_key),
        message_ids(&studio.bob_laptop, &ada_key),
        "both desktops read the conversation as one"
    );
    assert_eq!(
        direct(&studio.ada_laptop, &bob_key),
        direct(&studio.bob_laptop, &ada_key),
        "Bob's answer went into the conversation Ada opened, not a second one"
    );
}

/// A direct message is news on the other person's devices: Bob is told that Ada wrote to him, with
/// what she said, once on each device, and choosing the notice opens Chat in front.
#[test]
fn a_direct_message_is_one_notice_that_names_who_wrote() {
    let studio = Studio::start();
    let (ada_key, bob_key) = studio.keys();
    // Who Ada is here, as the bar's faces and a notice's words name her.
    let ada_name = person_label(&studio.bob_laptop, &ada_key);
    send(&studio.ada_laptop, &bob_key, "hello Bob");
    // Both of Bob's devices are told what Ada wrote: the words are hers, the notice names who
    // wrote as the workspace does, and it names the app it came from. His laptop reads the message
    // first, so a notice missing after that is the telling's, not the sync's.
    wait_for_texts(&studio.bob_laptop, &ada_key, &["hello Bob"]);
    let notice = wait_for_notice(&studio.bob_laptop, "hello Bob");
    assert!(
        notice["title"]
            .as_str()
            .is_some_and(|title| title.starts_with(&ada_name)),
        "the notice names {ada_name:?}, who wrote: {notice}"
    );
    assert_eq!(
        notice["app"],
        json!("Chat"),
        "the notice names the app it came from: {notice}"
    );
    let on_phone = wait_for_notice(&studio.bob_phone, "hello Bob");
    assert_eq!(
        on_phone["title"], notice["title"],
        "his phone is told the same words"
    );
    // Ada's own devices are not told what she wrote herself.
    for device in [&studio.ada_laptop, &studio.ada_phone] {
        assert!(
            notices_saying(device, "hello Bob").is_empty(),
            "{} is not told what Ada wrote herself: {:?}",
            device.label,
            notices(device)
        );
    }
    // Ada writes again: the second is news, and the first, which he was told of already, is not
    // told a second time.
    let sent = Instant::now();
    send(&studio.ada_laptop, &bob_key, "did you see?");
    // A message in a conversation already open is news the other person hears in moments, on
    // every device.
    wait_for_notices(&studio.bob_laptop, &["did you see?"]);
    wait_for_notices(&studio.bob_phone, &["did you see?"]);
    let heard = sent.elapsed();
    assert!(
        heard < MESSAGE_HEARD_WITHIN,
        "Bob is told Ada's second message {heard:?} after she sent it, not within \
         {MESSAGE_HEARD_WITHIN:?}"
    );
    wait_for_texts(&studio.bob_laptop, &ada_key, &["hello Bob", "did you see?"]);
    for device in [&studio.bob_laptop, &studio.bob_phone] {
        let told = wait_for_notices(device, &["hello Bob", "did you see?"]);
        for body in ["hello Bob", "did you see?"] {
            assert_eq!(
                told.iter()
                    .filter(|notice| notice["body"] == json!(body))
                    .count(),
                1,
                "{}: one notice says {body:?}, not two: {told:?}",
                device.label
            );
        }
        assert_eq!(
            told.len(),
            2,
            "{}: each message is one notice: {told:?}",
            device.label
        );
    }
    // Choosing the notice of her second message on his laptop, as the banner's button does,
    // brings Chat in front there.
    let second = wait_for_notice(&studio.bob_laptop, "did you see?");
    let other = studio.bob_laptop.window("so.tilo.notes", "In front");
    tilo_test_env::wait_until("another window in front of Chat", || {
        chat_windows(&studio.bob_laptop)
            .iter()
            .all(|window| window["active"] != json!(true))
    });
    choose(&studio.bob_laptop, &second);
    let deadline = tilo_test_env::hang_deadline();
    while !chat_windows(&studio.bob_laptop)
        .iter()
        .any(|window| window["active"] == json!(true))
    {
        assert!(
            Instant::now() < deadline,
            "choosing the notice never brings Chat forward: Chat shows {:?}\n{}",
            chat_windows(&studio.bob_laptop),
            studio.bob_laptop.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(other);
}

/// How long a message may take to reach the other person's devices, from the sender's send to the
/// notice on each of them. A loaded test machine delivers one in about two seconds; five allows
/// for that and still fails a stalled delivery.
const MESSAGE_HEARD_WITHIN: Duration = Duration::from_secs(5);

#[test]
fn messages_the_host_kept_reach_a_device_that_was_away() {
    let mut studio = Studio::start();
    let (ada_key, bob_key) = studio.keys();
    // Bob's devices are off while Ada writes: nothing of hers reaches them yet, and the site keeps
    // what she wrote.
    studio.bob_laptop.stop();
    studio.bob_phone.stop();
    for text in ["one", "two", "three"] {
        send(&studio.ada_laptop, &bob_key, text);
    }
    // Bob's laptop starts again: the workspace hands it all three, and his phone the same when it
    // comes back.
    start_together(&mut [&mut studio.bob_laptop, &mut studio.bob_phone]);
    for device in [&studio.bob_laptop, &studio.bob_phone] {
        show_chat(device, &studio.site);
        wait_for_texts(device, &ada_key, &["one", "two", "three"]);
    }
    // Bob answers, and Ada's laptop, which stayed on, shows it with her own three.
    send(&studio.bob_phone, &ada_key, "answered");
    let all = ["one", "two", "three", "answered"];
    for (device, peer) in [
        (&studio.ada_laptop, &bob_key),
        (&studio.ada_phone, &bob_key),
        (&studio.bob_laptop, &ada_key),
    ] {
        wait_for_texts(device, peer, &all);
    }
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
    send(&studio.ada_laptop, &bob_key, "Ada one");
    wait_for_texts(&studio.bob_laptop, &ada_key, &["Ada one"]);
    // Each person writes while the other's other device is away: neither pair of devices has heard
    // the other's message yet.
    studio.ada_phone.stop();
    studio.bob_phone.stop();
    send(&studio.ada_laptop, &bob_key, "Ada two");
    send(&studio.bob_laptop, &ada_key, "Bob one");
    send(&studio.bob_laptop, &ada_key, "Bob two");
    start_together(&mut [&mut studio.ada_phone, &mut studio.bob_phone]);
    for device in [&studio.ada_phone, &studio.bob_phone] {
        show_chat(device, &studio.site);
    }
    // Every message reaches every device, whichever order it reads them in.
    let wanted = ["Ada one", "Ada two", "Bob one", "Bob two"];
    for (device, peer) in [
        (&studio.ada_laptop, &bob_key),
        (&studio.ada_phone, &bob_key),
        (&studio.bob_laptop, &ada_key),
        (&studio.bob_phone, &ada_key),
    ] {
        wait_for_texts(device, peer, &wanted);
    }
    // Each message once, under one id, in one conversation, on every device.
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
    assert_eq!(
        directs(&studio.ada_laptop).len(),
        1,
        "the two of them have one conversation, however each wrote first"
    );
}

#[test]
fn one_read_mark_advances_once_and_both_of_a_persons_devices_agree() {
    let studio = Studio::start();
    let (ada_key, bob_key) = studio.keys();
    for text in ["one", "two", "three"] {
        send(&studio.ada_laptop, &bob_key, text);
    }
    // Bob has read none of Ada's on either device.
    for device in [&studio.bob_laptop, &studio.bob_phone] {
        wait_for_texts(device, &ada_key, &["one", "two", "three"]);
        wait_for_unread(device, &ada_key, 3);
    }
    // Reading them on his laptop marks them read for his phone too.
    open_conversation(&studio.bob_laptop, &ada_key);
    wait_for_unread(&studio.bob_laptop, &ada_key, 0);
    wait_for_unread(&studio.bob_phone, &ada_key, 0);
    // Read is not gone: both of his devices still show what Ada wrote.
    for device in [&studio.bob_laptop, &studio.bob_phone] {
        wait_for_texts(device, &ada_key, &["one", "two", "three"]);
    }
    // Ada's own messages are hers: nothing of hers is unread for herself.
    for device in [&studio.ada_laptop, &studio.ada_phone] {
        wait_for_texts(device, &bob_key, &["one", "two", "three"]);
        assert_eq!(unread(device, &bob_key), 0, "{}", device.label);
    }
}

#[test]
fn another_pair_is_another_conversation() {
    let studio = Studio::start();
    let (ada_key, bob_key) = studio.keys();
    send(&studio.ada_laptop, &bob_key, "just us");
    wait_for_texts(&studio.bob_laptop, &ada_key, &["just us"]);
    // Carol joins the same studio: what she reads with Bob is her own conversation with him, not
    // his with Ada.
    let carol = Person::new("Carol");
    let mut carol_laptop = carol.device("Carol's laptop");
    carol_laptop.install_dev_apps();
    enter(
        &mut carol_laptop,
        &studio.site,
        Some(studio.site.admit(carol.public).as_str()),
    );
    wait_for_member(&carol_laptop, &bob_key);
    // Reading whether she has a conversation with Bob makes none: she has none until she writes.
    assert_eq!(
        direct(&carol_laptop, &bob_key),
        None,
        "Carol has no conversation with Bob before she writes"
    );
    assert_eq!(
        direct(&carol_laptop, &bob_key),
        None,
        "reading it twice still made none"
    );
    assert!(
        refusal(&carol_laptop, &bob_key).is_none(),
        "Chat opens a conversation between Carol and Bob"
    );
    let theirs = direct(&carol_laptop, &bob_key).expect("the conversation Carol opened");
    assert_ne!(
        Some(theirs.clone()),
        direct(&studio.bob_laptop, &ada_key),
        "Carol and Bob's conversation is not Ada and Bob's"
    );
    // Their conversation holds nothing of Ada and Bob's.
    assert!(
        texts(&carol_laptop, &bob_key).is_empty(),
        "Carol reads nothing of the conversation between Ada and Bob"
    );
    send(&carol_laptop, &bob_key, "hi Bob, Carol here");
    wait_for_texts(
        &studio.bob_laptop,
        &hex::encode(carol.public),
        &["hi Bob, Carol here"],
    );
    // And a person the workspace never admitted is someone Chat refuses to open a conversation
    // with, in words she can read: a stranger is in no pair with her.
    let stranger = Person::new("Stranger");
    let refused = refusal(&carol_laptop, &hex::encode(stranger.public))
        .expect("Chat refuses a conversation with someone in no workspace with her");
    assert!(
        refused.contains("is not in this workspace"),
        "the refusal says why, in words a person reads: {refused}"
    );
    // And Bob's conversation with Ada shows nothing of Carol.
    assert_eq!(
        texts(&studio.bob_laptop, &ada_key),
        vec!["just us".to_owned()],
        "Bob's conversation with Ada is theirs"
    );
}

/// Bob's laptop holds a message his phone wrote under a key the laptop never took in, and the
/// workspace's host is gone: the phone, which holds that key, hands the laptop the group's frames
/// it lacks, and the laptop reads the message.
#[test]
fn a_device_takes_the_key_its_host_did_not_hand_it_from_the_persons_other_device() {
    let ada = Person::new("Ada");
    let bob = Person::new("Bob");
    let mut site = Site::start(&ada, "Studio");
    site.publish(Role::Editor);
    let mut bob_laptop = bob.device("Bob's laptop");
    let mut bob_phone = bob.device("Bob's phone");
    for device in [&bob_laptop, &bob_phone] {
        device.install_dev_apps();
    }
    let his = site.admit(bob.public);
    let his_phones = site.admit(bob.public);
    join_together(
        &mut [&mut bob_laptop, &mut bob_phone],
        &[his.as_str(), his_phones.as_str()],
    );
    for device in [&bob_laptop, &bob_phone] {
        show_chat(device, &site);
    }
    // The laptop's group state as it stands before the group moves on: what it comes back with
    // later, as a laptop whose last save of it never reached its disk does.
    bob_laptop.stop();
    let saved = tempfile::tempdir().unwrap();
    copy_dir(&group_state(&bob_laptop), saved.path());
    enter(&mut bob_laptop, &site, None);
    // Ada enters, which moves the workspace's group to a new key, and Bob's phone writes to her
    // under it. The laptop reads it while the host is there to hand it the key.
    let mut ada_laptop = ada.device("Ada's laptop");
    ada_laptop.install_dev_apps();
    enter(
        &mut ada_laptop,
        &site,
        Some(site.admit(ada.public).as_str()),
    );
    let (ada_key, bob_key) = (hex::encode(ada.public), hex::encode(bob.public));
    wait_for_member(&bob_phone, &ada_key);
    wait_for_member(&ada_laptop, &bob_key);
    send(&bob_phone, &ada_key, "under the new key");
    wait_for_texts(&bob_laptop, &ada_key, &["under the new key"]);
    // The host goes away, and the laptop comes back with its group as it saved it before Ada
    // came: it holds the message, not the key it is sealed under.
    bob_laptop.stop();
    ada_laptop.stop();
    site.stop();
    let state = group_state(&bob_laptop);
    std::fs::remove_dir_all(&state).unwrap();
    copy_dir(saved.path(), &state);
    bob_laptop.start();
    tilo_test_env::wait_for("Bob's laptop showing the studio again", || {
        bob_laptop.showing()
    });
    // Bob's phone, still on, hands it the key: the laptop reads the message with no host there.
    wait_for_texts(&bob_laptop, &ada_key, &["under the new key"]);
    // Each device reached the host and its other device from one endpoint: a server keeps one
    // connection per endpoint, and tells the older of two under one key that it was replaced.
    for device in [&bob_laptop, &bob_phone] {
        assert!(
            !device
                .log()
                .contains("Another endpoint connected with the same endpoint id"),
            "{} ran two endpoints under its key",
            device.label
        );
    }
}

/// Chat opens as the first app of a workspace made straight after the session restarted: through
/// the applications action, its window an app window of that workspace.
#[test]
fn chat_opens_first_in_a_workspace_made_after_the_session_restarted() {
    let ada = Person::new("Ada");
    let mut laptop = ada.device("Ada's laptop");
    laptop.install_dev_apps();
    laptop.start();
    tilo_test_env::wait_for("the laptop showing a workspace", || laptop.showing());
    laptop.stop();
    laptop.start();
    tilo_test_env::wait_for("the restarted laptop showing a workspace", || {
        laptop.showing()
    });
    let made = laptop.act_wait(
        &at("so.tilo.session", "desktop:chrome", "new-workspace"),
        &json!({"name": "Research"}),
    );
    assert!(
        made.get("err").is_none(),
        "a workspace of the person's: {made}"
    );
    tilo_test_env::wait_for("Research in front", || {
        laptop.showing().filter(|shown| !shown.is_empty())
    });
    laptop.open(CHAT);
    laptop.wait_for_window(CHAT);
    let windows = laptop.ask(&json!({"op": "windows"}));
    let chat = windows["answer"]["windows"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["app_id"] == CHAT))
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        (chat["kind"].as_str(), chat["workspace"].as_str()),
        (Some("Foreign"), Some("Research")),
        "Chat's window is an app window of Research: {windows}"
    );
}

/// Chat opens on a device alone, in a workspace the person makes on it (one this device keeps and
/// no site hosts) and in a branch of that workspace: the person opens it from the applications
/// list and its window comes.
#[test]
fn chat_opens_on_a_device_alone_in_a_workspace_it_made_and_in_its_branch() {
    let ada = Person::new("Ada");
    let mut laptop = ada.device("Ada's laptop");
    laptop.install_dev_apps();
    laptop.start();
    tilo_test_env::wait_for("the laptop showing a workspace", || laptop.showing());
    let made = laptop.act_wait(
        &at("so.tilo.session", "desktop:chrome", "new-workspace"),
        &json!({"name": "Research"}),
    );
    assert!(
        made.get("err").is_none(),
        "a workspace of the person's: {made}"
    );
    let research = tilo_test_env::wait_for("Research in front", || {
        laptop.showing().filter(|shown| !shown.is_empty())
    });
    laptop.open(CHAT);
    laptop.wait_for_window(CHAT);

    let branched = laptop.act_wait(
        &at("so.tilo.session", "desktop:chrome", "branch-workspace"),
        &json!({"source": research, "name": "Question A"}),
    );
    assert!(branched.get("err").is_none(), "the branch: {branched}");
    tilo_test_env::wait_for("the branch in front", || {
        laptop.showing().filter(|shown| *shown != research)
    });
    laptop.open(CHAT);
    laptop.wait_for_window(CHAT);
}

/// A running app is still offered to open, and opening it brings its window forward: what an
/// agent listing the workspace's actions finds is what the person's own launch does.
#[test]
fn a_running_apps_open_is_listed_and_brings_its_window_forward() {
    let ada = Person::new("Ada");
    let mut laptop = ada.device("Ada's laptop");
    laptop.install_dev_apps();
    laptop.start();
    tilo_test_env::wait_for("the laptop showing a workspace", || laptop.showing());
    laptop.open(CHAT);
    laptop.wait_for_window(CHAT);
    let _other = laptop.window("so.tilo.notes", "In front");
    tilo_test_env::wait_for("the other window in front of Chat", || {
        chat_windows(&laptop)
            .iter()
            .all(|row| row["active"] == false)
            .then_some(())
    });

    // Chat runs, and its open is listed all the same.
    let open = laptop.open_target(CHAT);
    let opened = laptop.act_wait(&open, &json!({}));
    assert!(opened.get("err").is_none(), "Chat's open: {opened}");
    tilo_test_env::wait_for("Chat's window in front", || {
        chat_windows(&laptop)
            .iter()
            .any(|row| row["active"] == true)
            .then_some(())
    });
    assert_eq!(
        chat_windows(&laptop).len(),
        1,
        "the open brought the one window forward and started no second"
    );
}

/// An idle studio wakes nothing: with the workspace, its people and Chat's windows all settled
/// and nothing typed, no device keeps a core busy and its collaboration broker stays asleep. A
/// held read re-checking on a clock, or a window drawing with nothing to show, is caught here.
/// The whole tree's wakeups are not the measure: a software-rendered session's rasteriser pool
/// wakes on every frame it is handed, so the CPU it spends and the broker thread's own wakeups
/// are.
#[test]
fn an_idle_studio_keeps_every_device_asleep() {
    let studio = Studio::start();
    for device in [
        &studio.ada_laptop,
        &studio.ada_phone,
        &studio.bob_laptop,
        &studio.bob_phone,
    ] {
        let pid = device.process().expect("the device's session runs");
        // Two settled seconds, after the studio's own startup has finished: a device that keeps
        // re-reading on a clock costs the same then as it did on its first frame.
        let idle =
            tilo_test_env::idle::idle_wakeups(pid, Duration::from_secs(2), Duration::from_secs(2))
                .expect("a process tree to sample");
        // One whole core is the complaint an idle device answers: an idle session costs a small
        // share of one, and a device re-reading its records on a clock costs several.
        assert!(
            idle.total_cpu_percent() < 100.0,
            "{} burns a core while idle: {idle}",
            device.label
        );
        let broker: u64 = idle
            .rows
            .iter()
            .filter(|row| row.thread == "tilo-collab-bro")
            .map(tilo_test_env::idle::WakeupRow::wakeups)
            .sum();
        assert!(
            idle.rate(broker) < 20.0,
            "{}: the collaboration broker keeps waking while idle: {idle}",
            device.label
        );
    }
}

/// Bring `device` into `site`'s workspace with Chat open: joining it with `member`, or starting
/// again in it when there is none. A device started beside others is started by that batch
/// ([`join_together`], [`start_together`]) and brought here by [`show_chat`].
fn enter(device: &mut Device, site: &Site, member: Option<&str>) {
    match member {
        Some(member) => device.join(member),
        None => device.start(),
    }
    show_chat(device, site);
}

/// Wait until `device`, whose session runs in `site`'s workspace, shows it in front with Chat's
/// window open in it and its actions published.
fn show_chat(device: &Device, site: &Site) {
    device.wait_holding(&site.workspace);
    device.wait_showing(&site.workspace);
    device.open(CHAT);
    device.wait_for_window(CHAT);
    conversations_action(device, "write_to");
}

/// Where `device` keeps the state of its workspaces' key groups.
fn group_state(device: &Device) -> std::path::PathBuf {
    device
        .directory()
        .join("state/tilo/collaboration/mls/groups")
}

/// Copy the directory `from`, whole, to `to`.
fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// How an actor argument names the person whose root key is `key`.
fn person(key: &str) -> Value {
    json!({ "person": key })
}

/// Chat's `action` on its conversations (the sidebar's resource) as `device` publishes it, once
/// it does.
fn conversations_action(device: &Device, action: &str) -> RemoteActionTarget {
    tilo_test_env::wait_for(
        format!("{} publishing Chat's {action}", device.label),
        || {
            device
                .actions()
                .iter()
                .find(|row| {
                    row.app_id == CHAT
                        && row.resource.kind.ends_with(".Conversations")
                        && row
                            .action
                            .as_ref()
                            .is_some_and(|offered| offered.address.action == action)
                })
                .map(target)
        },
    )
}

/// Chat's `action` on the resource `resource` as `device` publishes it, once it does.
fn action_on(device: &Device, resource: &str, action: &str) -> RemoteActionTarget {
    tilo_test_env::wait_for(
        format!("{} publishing {action} of {resource}", device.label),
        || device.offered(resource, action),
    )
}

/// Invoke `action` on `device` with `args`: what it answered once it settled, `Ok` with the
/// output of one that succeeded and `Err` with the words of one that did not.
fn invoke(device: &Device, action: &RemoteActionTarget, args: &Value) -> Result<Value, String> {
    let answer = device.act_wait(action, args);
    if let Some(error) = answer.get("err") {
        return Err(error["message"].as_str().unwrap_or_default().to_owned());
    }
    match answer["update"]["state"].as_str() {
        Some("succeeded") => Ok(answer["update"]["output"].clone()),
        Some("failed" | "cancelled") => Err(answer["update"].to_string()),
        other => panic!(
            "{} {:?} ended as {other:?}: {answer}\n{}",
            device.label,
            action.address,
            device.diagnostic()
        ),
    }
}

/// Write `text` to the person whose root key is `peer`, from `device`, as Chat's `write_to` does
/// for a person, an agent or a script.
fn send(device: &Device, peer: &str, text: &str) {
    let write = conversations_action(device, "write_to");
    if let Err(why) = invoke(device, &write, &json!({"who": person(peer), "body": text})) {
        panic!(
            "{} writes {text:?} to {peer}: {why}\n{}",
            device.label,
            device.diagnostic()
        );
    }
}

/// The resource an output names: what an action that answers a resource answered, none when it
/// answered nothing.
fn answered_resource(output: &Value) -> Option<String> {
    output["resources"]
        .as_array()
        .and_then(|made| made.first())
        .or_else(|| Some(&output["value"]))
        .and_then(|value| {
            value
                .as_str()
                .or_else(|| value["id"].as_str())
                .map(str::to_owned)
        })
        .filter(|id| !id.is_empty())
}

/// The direct conversation `device`'s person has with the person whose root key is `peer`, as
/// Chat's `conversation_with` reads it: none while they have none. It makes nothing.
fn direct(device: &Device, peer: &str) -> Option<String> {
    let read = conversations_action(device, "conversation_with");
    let output = invoke(device, &read, &json!({"who": person(peer)}))
        .unwrap_or_else(|why| panic!("{} reads its conversation with {peer}: {why}", device.label));
    answered_resource(&output)
}

/// Every direct conversation `device` lists.
fn directs(device: &Device) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for row in device.actions() {
        if row.app_id == CHAT && row.resource.kind == DIRECT && !found.contains(&row.resource.id) {
            found.push(row.resource.id);
        }
    }
    found
}

/// Wait until `device` reads a conversation with the person whose root key is `peer`.
fn wait_for_direct(device: &Device, peer: &str) -> String {
    tilo_test_env::wait_for(
        format!("{}'s conversation with {peer}", device.label),
        || direct(device, peer),
    )
}

/// The messages of the conversation with `peer` as `device` lists them, each once: its id and the
/// first line of what it says. None while there is no conversation.
fn messages(device: &Device, peer: &str) -> Vec<(String, String)> {
    let Some(conversation) = direct(device, peer) else {
        return Vec::new();
    };
    let mut found: Vec<(String, String)> = Vec::new();
    for row in device.actions() {
        if row.app_id == CHAT
            && row.resource.kind == MESSAGE
            && row.parent_resource == conversation
            && !found.iter().any(|(id, _)| *id == row.resource.id)
        {
            found.push((row.resource.id, row.resource.label));
        }
    }
    found
}

/// What the messages of the conversation with `peer` say, as `device` lists them.
fn texts(device: &Device, peer: &str) -> Vec<String> {
    messages(device, peer)
        .into_iter()
        .map(|(_, text)| text)
        .collect()
}

/// The ids of the messages of the conversation with `peer`, as `device` lists them, sorted: two
/// devices that read the same messages answer the same list.
fn message_ids(device: &Device, peer: &str) -> Vec<String> {
    let mut ids: Vec<String> = messages(device, peer)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    ids.sort();
    ids
}

/// Wait until the conversation with `peer` on `device` says exactly `wanted`, in whichever order
/// the device lists them.
fn wait_for_texts(device: &Device, peer: &str, wanted: &[&str]) {
    let mut wanted: Vec<String> = wanted.iter().map(|text| (*text).to_owned()).collect();
    wanted.sort();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let mut shown = texts(device, peer);
        shown.sort();
        if shown == wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{}'s conversation with {peer} never says {wanted:?}; it says {shown:?}\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// How many messages of the conversation with `peer` `device`'s person has not read, as Chat's
/// `unread_count` reads it.
fn unread(device: &Device, peer: &str) -> i64 {
    let conversation = wait_for_direct(device, peer);
    let count = action_on(device, &conversation, "unread_count");
    let output = invoke(device, &count, &json!({}))
        .unwrap_or_else(|why| panic!("{} reads what is unread: {why}", device.label));
    output["value"]
        .as_i64()
        .unwrap_or_else(|| panic!("{} answers a count: {output}", device.label))
}

/// Wait until `device` reads `wanted` messages of the conversation with `peer` unread.
fn wait_for_unread(device: &Device, peer: &str, wanted: i64) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let now = unread(device, peer);
        if now == wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{} reads {now} unread with {peer}, never {wanted}\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Open the conversation with `peer` in a window on `device`, as a person choosing it in Chat's
/// sidebar does: the layout's `open` of the conversation.
fn open_conversation(device: &Device, peer: &str) {
    let conversation = wait_for_direct(device, peer);
    let open = action_on(device, "shell:layout", "open");
    let answer = device.act_wait(&open, &json!({"resource": conversation}));
    assert!(
        answer.get("err").is_none() && answer["update"]["state"] != json!("failed"),
        "{} opens its conversation with {peer}: {answer}",
        device.label
    );
}

/// Why Chat on `device` refuses to open a conversation with the person whose root key is `peer`,
/// none when it opens one (its `message_person`, as the sidebar's Message does).
fn refusal(device: &Device, peer: &str) -> Option<String> {
    let message = conversations_action(device, "message_person");
    invoke(device, &message, &json!({"who": person(peer)})).err()
}

/// Wait until `device`'s session names the person whose root key is `key` among the members of
/// its workspace.
fn wait_for_member(device: &Device, key: &str) {
    tilo_test_env::wait_for(format!("{} naming {key}", device.label), || {
        member_label(device, key)
    });
}

/// The name `device`'s session gives the person whose root key is `key`, as the bar's faces and a
/// notice's words name them, once it names them.
fn member_label(device: &Device, key: &str) -> Option<String> {
    device.state()["state"]["peers"]["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|workspace| workspace["members"].as_array().cloned().unwrap_or_default())
        .find(|member| member["user"].as_str() == Some(key))
        .and_then(|member| member["label"].as_str().map(str::to_owned))
        .filter(|label| !label.is_empty())
}

/// The name `device`'s session gives the person whose root key is `key`.
fn person_label(device: &Device, key: &str) -> String {
    member_label(device, key)
        .unwrap_or_else(|| panic!("{} gives {key} no name: {}", device.label, device.state()))
}

/// Choose `notice` on `device`, as pressing its button does: the notifications' own `act`, by
/// the id the state dump lists the notice under, with the notice's open (its first action).
fn choose(device: &Device, notice: &Value) {
    let key = notice["actions"]
        .as_array()
        .and_then(|actions| {
            actions
                .iter()
                .find(|action| action["key"] == json!("open"))
                .or_else(|| actions.first())
        })
        .and_then(|action| action["key"].as_str())
        .unwrap_or("default")
        .to_owned();
    let act = action_on(device, "session:notifications", "act");
    let answer = device.act_wait(&act, &json!({"notification": notice["id"], "action": key}));
    assert!(
        answer.get("err").is_none(),
        "{} chooses {key} on {notice}: {answer}",
        device.label
    );
}

/// This device's Chat windows as the session lists them: each row's title, and whether it is the
/// one in front of its group.
fn chat_windows(device: &Device) -> Vec<Value> {
    device.ask(&json!({"op": "windows"}))["answer"]["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|window| window["app_id"] == json!(CHAT))
        .map(|window| json!({"title": window["title"], "active": window["active"]}))
        .collect()
}

/// The notifications this device's session shows now, as its state dump lists them, each once:
/// the banners up and what the centre keeps.
fn notices(device: &Device) -> Vec<Value> {
    let mut shown: Vec<Value> = Vec::new();
    for notice in device.state()["state"]["notifications"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        if !shown.iter().any(|seen| seen["id"] == notice["id"]) {
            shown.push(notice);
        }
    }
    shown
}

/// The notices this device's session shows that say `body`: a person's words are what a notice is
/// found by, whatever they are called here.
fn notices_saying(device: &Device, body: &str) -> Vec<Value> {
    notices(device)
        .into_iter()
        .filter(|notice| notice["body"] == json!(body))
        .collect()
}

/// Wait until this device's session shows exactly one notice saying `body`, and answer it; what it
/// shows instead is the failure's message.
fn wait_for_notice(device: &Device, body: &str) -> Value {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let found = notices_saying(device, body);
        if found.len() == 1 {
            return found[0].clone();
        }
        assert!(
            Instant::now() < deadline,
            "{} is never told {body:?} once; its session shows {:?}\n{}",
            device.label,
            notices(device),
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Wait until this device's session shows a notice saying each of `bodies`, and answer those
/// notices.
fn wait_for_notices(device: &Device, bodies: &[&str]) -> Vec<Value> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let shown = notices(device);
        let found: Vec<Value> = shown
            .iter()
            .filter(|notice| bodies.iter().any(|body| notice["body"] == json!(*body)))
            .cloned()
            .collect();
        if bodies
            .iter()
            .all(|body| found.iter().any(|notice| notice["body"] == json!(*body)))
        {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "{} is never told {bodies:?}; its session shows {shown:?}\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
