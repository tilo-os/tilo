#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixtures: a failed step is the assertion"
)]

//! An app's presence over the connection its launcher hands it: what one of its views says
//! leaves for the workspace's other apps and devices, what the workspace's live session hears
//! reaches those views, a refused word says nothing, and a window that closes takes its caret
//! with it.

use std::sync::{Arc, Mutex};

use collab_types::MemberId;
use collab_types::presence::{Heard, Payload, Persona, Presence, PresenceKey, Said};
use tilo_action_line::LineClient;

use crate::RuntimeServices as _;
use crate::device::live_speaking::Watch;
use crate::device::services::Daemon;
use crate::device::services::tests::isolated_root;

const CARET_TEST: &str =
    "device::app_presence::tests::a_caret_said_on_one_apps_line_reaches_the_others";
const LEFT_TEST: &str = "device::app_presence::tests::an_app_that_leaves_takes_its_caret_with_it";
const HEARD_TEST: &str = "device::app_presence::tests::another_members_word_reaches_the_apps_views";
const REFUSED_TEST: &str =
    "device::app_presence::tests::a_word_the_live_presence_refuses_is_said_to_nobody";
const PERSONA_TEST: &str =
    "device::app_presence::tests::an_apps_persona_types_as_the_apps_and_never_as_its_person";

const APP: &str = "so.tilo.example.shared-checklist";

fn caret_key() -> PresenceKey {
    PresenceKey::resource(APP, "item-1", "Item")
}

/// What the apps of a workspace were handed: member, the app's persona it is, and word.
type Words = Arc<Mutex<Vec<(MemberId, Option<Persona>, Presence)>>>;

/// The device's daemon, its workspace, and what an app's views hear there.
fn device(test: &str) -> Option<(Arc<Daemon>, String, Words)> {
    let root = isolated_root(test)?;
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let workspace = daemon.workspace().expect("the person's workspace");
    let heard: Words = Arc::default();
    Some((daemon, workspace, heard))
}

/// An app's connection, and the words its views are handed, as its lane writes them.
fn app(daemon: &Arc<Daemon>, workspace: &str, heard: &Words) -> (LineClient, Watch) {
    let live = Arc::clone(daemon.live().expect("the daemon runs its live session"));
    let sink = Arc::clone(heard);
    // What an app's own lane registers: a watcher of the workspace's words, as its views are
    // handed them.
    let watch = live
        .speaking
        .watch_app(workspace, move |member, persona, frame| {
            sink.lock().unwrap().push((member, persona, frame));
        });
    let connection = LineClient::of_stream(
        daemon
            .app_connection_for_test(APP, workspace)
            .expect("the daemon hands the app its connection"),
    )
    .expect("the app's connection reads");
    (connection, watch)
}

fn said(key: &PresenceKey, payload: Payload) -> serde_json::Value {
    serde_json::to_value(Said {
        key: key.clone(),
        payload,
        persona: None,
    })
    .expect("a word writes")
}

#[test]
fn a_caret_said_on_one_apps_line_reaches_the_others() {
    let Some((daemon, workspace, heard)) = device(CARET_TEST) else {
        return;
    };
    let (connection, _watch) = app(&daemon, &workspace, &heard);
    let member = MemberId::person(daemon.person_id());
    connection
        .say_presence(&said(&caret_key(), Payload::Caret { pos: 7 }))
        .expect("the live presence says the app's word");
    let words = heard.lock().unwrap();
    let (from, persona, frame) = words.last().expect("the other apps are handed the word");
    assert_eq!(*persona, None, "the person's own word");
    assert_eq!(*from, member, "attributed to the connection's member");
    assert_eq!(frame.app, APP);
    assert_eq!(frame.cluster, workspace);
    assert_eq!(frame.key, caret_key());
    assert_eq!(frame.payload, Payload::Caret { pos: 7 });
    drop(words);
}

#[test]
fn an_app_that_leaves_takes_its_caret_with_it() {
    let Some((daemon, workspace, heard)) = device(LEFT_TEST) else {
        return;
    };
    let (connection, _watch) = app(&daemon, &workspace, &heard);
    connection
        .say_presence(&said(&caret_key(), Payload::Caret { pos: 3 }))
        .expect("the live presence says the app's word");
    assert_eq!(
        heard
            .lock()
            .unwrap()
            .last()
            .map(|(_, _, frame)| frame.payload.clone()),
        Some(Payload::Caret { pos: 3 })
    );
    drop(connection);
    tilo_test_env::wait_until("the closed window's caret to leave", || {
        heard
            .lock()
            .unwrap()
            .last()
            .is_some_and(|(_, _, frame)| frame.payload == Payload::Left)
    });
    assert_eq!(
        heard
            .lock()
            .unwrap()
            .last()
            .map(|(_, _, frame)| frame.payload.clone()),
        Some(Payload::Left),
        "a window that closed leaves its caret"
    );
}

#[test]
fn another_members_word_reaches_the_apps_views() {
    let Some((daemon, workspace, _heard)) = device(HEARD_TEST) else {
        return;
    };
    let live = Arc::clone(daemon.live().expect("the daemon runs its live session"));
    let connection = LineClient::of_stream(
        daemon
            .app_connection_for_test(APP, &workspace)
            .expect("the daemon hands the app its connection"),
    )
    .expect("the app's connection reads");
    // The connection is served once the app's first call is read, and only then does its lane
    // hear the workspace's words: say one first.
    connection
        .say_presence(&said(&caret_key(), Payload::Caret { pos: 0 }))
        .expect("the app's first call is read");
    let ada = MemberId::person("ada");
    let frame = Presence::app(
        APP.to_owned(),
        workspace,
        caret_key(),
        Payload::Caret { pos: 11 },
    );
    live.speaking.hear_from_other_device(&ada, &frame);
    let mut heard = connection.presence_heard();
    tilo_test_env::wait_until("the view to hear the word", || {
        if heard.is_empty() {
            heard = connection.presence_heard();
        }
        !heard.is_empty()
    });
    let heard: Heard = serde_json::from_str(heard.first().expect("the view hears the word"))
        .expect("a word the view reads");
    assert_eq!(heard.member, ada);
    assert_eq!(heard.key, caret_key());
    assert_eq!(heard.payload, Payload::Caret { pos: 11 });
}

#[test]
fn a_word_the_live_presence_refuses_is_said_to_nobody() {
    let Some((daemon, workspace, heard)) = device(REFUSED_TEST) else {
        return;
    };
    let (connection, _watch) = app(&daemon, &workspace, &heard);
    let refused = connection.say_presence(&said(
        &PresenceKey::Workspace { id: workspace },
        Payload::Availability {
            state: "away".to_owned(),
            call: None,
            camera: false,
            screen: false,
        },
    ));
    assert!(
        refused.is_err(),
        "the desktop's own word is the shell's, never an app's"
    );
    assert!(
        heard.lock().unwrap().is_empty(),
        "a refused word is handed to nobody"
    );
}

/// A bridge says Dana, whom it carries in from Discord, types: the other apps are handed the
/// word as the bridge's persona, never as the person whose device runs the bridge, and the
/// typing leaves with the bridge. A persona says what someone does, never a status.
#[test]
fn an_apps_persona_types_as_the_apps_and_never_as_its_person() {
    let Some((daemon, workspace, heard)) = device(PERSONA_TEST) else {
        return;
    };
    let (connection, _watch) = app(&daemon, &workspace, &heard);
    let dana = Persona {
        service: "discord".into(),
        account: "3001".into(),
        name: "Dana".into(),
    };
    let as_dana = |payload: Payload| {
        serde_json::to_value(Said {
            key: caret_key(),
            payload,
            persona: Some(dana.clone()),
        })
        .expect("a word writes")
    };
    connection
        .say_presence(&as_dana(Payload::Composing { composing: true }))
        .expect("the live presence says the persona's word");
    let (from, persona, frame) = heard
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("the other apps are handed the word");
    assert_eq!(from, MemberId::bot(format!("{APP}/discord:3001")));
    assert_ne!(from, MemberId::person(daemon.person_id()));
    assert_eq!(
        persona.as_ref().map(Persona::shown).as_deref(),
        Some("Dana (Discord)")
    );
    assert_eq!(frame.payload, Payload::Composing { composing: true });

    let refused = connection.say_presence(&as_dana(Payload::Status {
        text: "Out".into(),
        automatic: false,
    }));
    assert!(
        refused.is_err(),
        "a status is a member's, never a persona's"
    );
    drop(connection);
    tilo_test_env::wait_until("the persona's typing to leave with the bridge", || {
        heard
            .lock()
            .unwrap()
            .last()
            .is_some_and(|(from, _, frame)| frame.payload == Payload::Left && from.is_bot())
    });
}
