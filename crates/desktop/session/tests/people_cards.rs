//! A person's card on the tilolang shell: what someone else's card says of them and offers, and
//! the person's own card — their colour, their settings, their picture, and what they type there.
//!
//! Every test drives the shell the session draws, through the controls assistive technology hears,
//! as a person with a pointer and a keyboard does.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::time::Duration;

use serde_json::json;
use tilo_compositor::{KeyRepeat, OutputConfig};
use tilo_desktop_search::providers::AppEntry;
use tilo_session::{AppLists, Lists, Options, Session};
use tilo_session_core::desktop::here::LiveMember;

use crate::common::{KEY_ENTER, PERSON, key, press, type_text, wait_for};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
/// The Backspace key, as the compositor's keyboard reads it.
const KEY_BACKSPACE: u32 = 14;

/// A session that does not repeat a held key: a frame on a loaded machine can outlast the repeat
/// delay, and a test never holds one.
fn laid_out(session: &mut Session) {
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(1),
        rate: 0,
    });
}

/// This device's shell with the apps `applications` installed here.
fn session_with(applications: Vec<AppEntry>) -> Session {
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        lists: Lists::fixed(AppLists {
            applications,
            ..AppLists::default()
        }),
        ..Options::default()
    });
    laid_out(&mut session);
    session
}

/// A session with no app installed here.
fn session() -> Session {
    session_with(Vec::new())
}

/// The app that holds a person's conversations, as this device's lists name it.
fn chat() -> AppEntry {
    AppEntry {
        id: "so.tilo.chat".to_owned(),
        title: "Chat".to_owned(),
        category: None,
    }
}

/// Who is here, as the peer network lists them: the members, each its own device. The shell is up
/// first: presence written into a session still starting is what its own start replaces.
fn set_here(session: &mut Session, members: Vec<LiveMember>) {
    wait_for(session, "the bar", |session| {
        session.shell_control("Top bar")
    });
    session
        .sync()
        .expect("the session takes part in the app wire")
        .here()
        .borrow_mut()
        .set_members(None, Some(members));
}

/// Someone here on `device`, by the root key they are here by.
fn someone(publisher: &str, name: &str, device: &str, root: &str) -> LiveMember {
    LiveMember {
        publisher: publisher.to_owned(),
        kind: "person".to_owned(),
        label: name.to_owned(),
        device: device.to_owned(),
        user: Some(root.to_owned()),
        availability: Some("available".to_owned()),
        ..LiveMember::default()
    }
}

/// An agent here, as the workspace admits it, at what it says it is doing.
fn an_agent(publisher: &str, name: &str, doing: &str) -> LiveMember {
    LiveMember {
        publisher: publisher.to_owned(),
        kind: "agent".to_owned(),
        label: name.to_owned(),
        member: Some(tilo_actions::MemberId::agent(name.to_lowercase())),
        activity: Some(doing.to_owned()),
        ..LiveMember::default()
    }
}

/// The session's reading of who is here, as the shell reads it.
fn people(session: &Session) -> serde_json::Value {
    session.read("session:people").expect("session:people")
}

/// Open who is here, where every person and agent has a row, so a press on their name opens their
/// card wherever their face does or does not stand on the bar.
fn open_who_is_here(session: &mut Session) {
    wait_for(session, "the bar", |session| {
        session.shell_control("Top bar")
    });
    session
        .invoke("session:people", "show", &serde_json::Map::new())
        .expect("who is here opens");
    wait_for(session, "who is here", |session| {
        session.shell_control("Invite someone")
    });
}

/// Wait until the shell shows the control `name`.
fn shows(session: &mut Session, name: &str) {
    wait_for(session, format!("the shell's {name}"), |session| {
        session.shell_control(name)
    });
}

/// Wait until the shell says `words` anywhere — a row's own line, a face's name or a row's quiet
/// fact, which no control of its own carries.
fn says(session: &mut Session, words: &str) {
    wait_for(session, format!("the shell to say {words:?}"), |session| {
        session.shell_says(words).then_some(())
    });
}

/// The shell shows no control `name`, and says nothing of it: what a card never offers where the
/// host gives it nothing to offer.
fn never_shows(session: &Session, name: &str, why: &str) {
    assert!(
        session.shell_control(name).is_none()
            && !session
                .accessible_labels()
                .iter()
                .any(|label| label == name),
        "{why}: {:?}",
        session.accessible_labels()
    );
}

/// Open the person's own card from their face on the bar.
fn open_own_card(session: &mut Session) {
    let face = wait_for(session, "the bar's own face", |session| {
        session
            .accessible_labels()
            .into_iter()
            .find(|label| label.ends_with(": open your card"))
    });
    press(session, &face);
    shows(session, "Add a device");
}

/// The person's identity as this device keeps it, with no name given: the state a card asks for a
/// name in.
fn make_the_identity_unnamed(home: &tilo_test_env::TestHome) {
    // The account's own store below the identity directory: each account of a computer keeps one.
    let dir =
        tilo_identity::store::account_identity_dir(&home.config().join("tilo").join("identity"));
    let identity = tilo_identity::store::resolve_active_identity(&dir).expect("an active identity");
    let id = identity
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .expect("an identity id")
        .to_owned();
    tilo_identity::store::update_identity_profile(
        &dir,
        &id,
        tilo_identity::store::UNNAMED_DISPLAY_NAME,
        [7, 7, 7],
    )
    .expect("the stand-in name");
}

/// A session on this device with an identity whose name nobody has given yet.
fn unnamed_session() -> (tilo_test_env::TestHome, Session) {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let state = crate::common::device_in(&mut home);
    make_the_identity_unnamed(&home);
    // What the session's own start does on a device: the person's identity, read from it and kept
    // current from it.
    tilo_session::identity::install();
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            state: Some(state),
            person: PERSON.to_owned(),
            ..Options::default()
        },
    );
    laid_out(&mut session);
    (home, session)
}

/// Wait until what the person says they are doing is `doing`.
fn says_they_are_doing(session: &mut Session, doing: &str) {
    wait_for(session, format!("the person says {doing:?}"), |session| {
        (people(session)["me"]["doing"] == json!(doing)).then_some(())
    });
}

/// The name the person goes by, as the reading says it.
fn name_of(session: &Session) -> String {
    people(session)["me"]["name"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn a_message_is_offered_only_where_chat_and_the_persons_key_are_here() {
    let _settled = ui::motion::testing::settled();
    // Chat is installed, so the card of someone whose key is known offers their conversation.
    let mut session = session_with(vec![chat()]);
    let mia = someone(&"01".repeat(32), "Mia Clayton", "Studio laptop", "mia-root");
    // Someone the host knows no key for — a guest, before their line arrives — has no record a
    // conversation with them is kept on.
    let sam = LiveMember {
        publisher: "02".repeat(32),
        kind: "person".to_owned(),
        label: "Sam Okafor".to_owned(),
        ..LiveMember::default()
    };
    set_here(&mut session, vec![mia, sam]);
    open_who_is_here(&mut session);
    press(&mut session, "Mia Clayton");
    shows(&mut session, "Message");
    press(&mut session, "Back to everyone");
    press(&mut session, "Sam Okafor");
    shows(&mut session, "Wave");
    never_shows(
        &session,
        "Message",
        "a conversation needs a key this host has not heard",
    );

    // Without Chat here there is no conversation to open either, whoever the person is.
    let mut without = session_with(Vec::new());
    set_here(
        &mut without,
        vec![someone(
            &"01".repeat(32),
            "Mia Clayton",
            "Studio laptop",
            "mia-root",
        )],
    );
    open_who_is_here(&mut without);
    press(&mut without, "Mia Clayton");
    shows(&mut without, "Wave");
    never_shows(
        &without,
        "Message",
        "without Chat here there is no conversation to open",
    );
}

#[test]
fn a_persons_card_lists_their_devices_and_the_accounts_they_keep_elsewhere() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    // One person here on two of their devices, one of them away, with an account they keep at a
    // service this workspace is connected to.
    let laptop = someone(&"01".repeat(32), "Mia Clayton", "Studio laptop", "mia-root");
    let mut desktop = someone(&"02".repeat(32), "Mia Clayton", "Attic desktop", "mia-root");
    desktop.availability = Some("away".to_owned());
    set_here(&mut session, vec![laptop, desktop]);
    assert!(
        session
            .sync()
            .expect("the app wire")
            .here()
            .borrow_mut()
            .set_linked(
                "mia-root",
                vec![tilo_app::linked_accounts::ProfileLink {
                    service: "Discord".to_owned(),
                    handle: "ada".to_owned(),
                    picture: None,
                }],
            ),
        "the accounts the person's profile names arrived"
    );
    open_who_is_here(&mut session);
    press(&mut session, "Mia Clayton");
    // Each device the person is here on, in the order it answers, saying how available they are
    // there; and the account their profile names at the service.
    says(&mut session, "Studio laptop");
    says(&mut session, "Attic desktop");
    says(&mut session, "Away");
    says(&mut session, "Linked to @ada on Discord");
}

#[test]
fn an_agents_card_says_what_it_is_at_and_offers_what_can_be_asked_of_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    set_here(
        &mut session,
        vec![an_agent(
            &"03".repeat(32),
            "Fieldwork",
            "Editing the launch note",
        )],
    );
    // Who is here, where the agent has its row, and a press on the row opens its card.
    open_who_is_here(&mut session);
    press(&mut session, "Fieldwork");
    // What it is at, as its live line, and what can be asked of it: Stop, Undo run, Take over and
    // Revoke, each carried out by its card in Console.
    says(&mut session, "Editing the launch note");
    for ask in [
        "Stop",
        "Undo run",
        "Take over",
        "Revoke access",
        "See its work",
    ] {
        shows(&mut session, ask);
    }
    // An agent is followed like anyone here; a conversation is not one, though.
    shows(&mut session, "Follow");
    never_shows(&session, "Message", "a conversation is never an agent's");

    // A press on Stop asks its card, which this device does not serve: the ask is taken, and the
    // card stays open to show what came of it.
    press(&mut session, "Stop");
    assert!(
        session.shell_refusals().is_empty(),
        "the ask is not refused by the reading: {:?}",
        session.shell_refusals()
    );
    shows(&mut session, "Stop");

    // What the card never offers is refused where a script asks it anyway: the verbs are the
    // resource's, and one it does not name is no ask at all.
    let refused = session.invoke(
        "session:people",
        "agent",
        &serde_json::Map::from_iter([
            ("person".to_owned(), json!("03".repeat(32))),
            ("client".to_owned(), json!("Fieldwork")),
            ("ask".to_owned(), json!("dance")),
        ]),
    );
    let said = format!("{:?}", refused.expect_err("an ask the card does not offer"));
    assert!(
        said.contains("stop") && said.contains("take-over"),
        "it is told what can be asked: {said}"
    );
}

#[test]
fn the_own_card_offers_what_the_host_offers_and_wears_the_colour_chosen() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    open_own_card(&mut session);
    // Settings at the card's top, everything that acts as the person beside it, and the switch
    // the desktop's control hints have.
    for control in [
        "Settings",
        "Add a device",
        "Devices and access",
        "Show control hints",
    ] {
        shows(&mut session, control);
    }
    // Nothing on this host offers a picture, so neither picture control stands.
    never_shows(
        &session,
        "Change picture",
        "no host action offers a picture",
    );
    never_shows(
        &session,
        "Remove picture",
        "no host action offers a picture",
    );

    // The colour is the person's, by the id the reading names it: choosing one wears it, and the
    // card marks the one the face is drawn in.
    session
        .invoke(
            "session:people",
            "colour",
            &serde_json::Map::from_iter([("slot".to_owned(), json!(9))]),
        )
        .expect("a colour the card offers");
    wait_for(&mut session, "the colour to be worn", |session| {
        (people(session)["me"]["colour_slot"] == json!(9)).then_some(())
    });
    assert_eq!(people(&session)["me"]["colour_id"], json!("violet"));
    // The swatches are the twelve people colours, in the order a face is drawn in them.
    assert_eq!(
        people(&session)["colours"].as_array().map(Vec::len),
        Some(12)
    );

    // The control hints' switch is the person's record, which every device reads.
    press(&mut session, "Show control hints");
    wait_for(&mut session, "the hints to be off", |session| {
        (people(session)["control_hints"] == json!(false)).then_some(())
    });
}

#[test]
fn a_picture_is_offered_where_the_host_lists_its_actions() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let action = |id: &str, title: &str| tilo_session_core::desktop::state::HostAction {
        id: id.to_owned(),
        title: title.to_owned(),
        label: None,
        subtitle: None,
        icon: None,
        destructive: false,
        unasked: false,
        source: Some(tilo_session_core::desktop::state::HostActionSource::Identity),
    };
    session.handle().set_host_actions(vec![
        action("set-photo", "Set a picture"),
        action("reset-photo", "Take the picture away"),
    ]);
    open_own_card(&mut session);
    // The picture's own actions stand beside the face, and the face itself is the button that
    // changes it.
    shows(&mut session, "Change picture");
    shows(&mut session, "Remove picture");
    press(&mut session, "Change picture");
    assert!(
        session.shell_refusals().is_empty(),
        "the host's own action is taken: {:?}",
        session.shell_refusals()
    );
}

#[test]
fn a_typed_status_is_kept_when_the_card_closes() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    open_own_card(&mut session);
    press(&mut session, "What you're doing");
    type_text(&mut session, "Reviewing the signup");
    // Nothing is kept while it is typed.
    assert_eq!(people(&session)["me"]["doing"], json!(""));
    press(&mut session, "Close");
    says_they_are_doing(&mut session, "Reviewing the signup");
    // A card reopening holds what was kept, and what is said again is kept again rather than
    // compared with what it first held.
    open_own_card(&mut session);
    press(&mut session, "What you're doing");
    for _ in 0..64 {
        key(&mut session, KEY_BACKSPACE);
    }
    type_text(&mut session, "Drafting the launch");
    press(&mut session, "Close");
    says_they_are_doing(&mut session, "Drafting the launch");
}

#[test]
fn a_pause_after_typing_keeps_what_was_typed() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    open_own_card(&mut session);
    press(&mut session, "What you're doing");
    type_text(&mut session, "Reviewing the signup");
    assert_eq!(
        people(&session)["me"]["doing"],
        json!(""),
        "not while it is typed"
    );
    // Typing stops, and the pause that follows keeps it without a Save: the session wakes for it.
    session.step(Duration::from_millis(900)).expect("step");
    says_they_are_doing(&mut session, "Reviewing the signup");
}

#[test]
fn enter_keeps_what_is_typed_and_the_card_stays() {
    let _settled = ui::motion::testing::settled();
    let (_home, mut session) = unnamed_session();
    open_own_card(&mut session);
    press(&mut session, "Add your name");
    type_text(&mut session, "Ada");
    key(&mut session, KEY_ENTER);
    wait_for(&mut session, "the name they gave", |session| {
        (name_of(session) == "Ada").then_some(())
    });
    shows(&mut session, "Add a device");
}

#[test]
fn a_name_is_kept_when_the_card_closes_and_a_blank_or_unchanged_one_changes_nothing() {
    let _settled = ui::motion::testing::settled();
    let (_home, mut session) = unnamed_session();
    open_own_card(&mut session);
    // No name given yet: the field asks for one and holds nothing to type over.
    shows(&mut session, "Add your name");
    let before = name_of(&session);
    assert_ne!(before, "Ada", "the name before was nobody's");
    press(&mut session, "Add your name");
    type_text(&mut session, "Ada");
    press(&mut session, "Close");
    wait_for(&mut session, "the name they gave", |session| {
        (name_of(session) == "Ada").then_some(())
    });

    // Opening the card again, the field holds the name they gave.
    open_own_card(&mut session);
    shows(&mut session, "Your name");

    // A name left as it is, and a blank answer: neither is a name to keep, so neither loses the
    // one they have.
    press(&mut session, "Your name");
    press(&mut session, "Close");
    wait_for(&mut session, "the name to stay", |session| {
        (name_of(session) == "Ada").then_some(())
    });
    open_own_card(&mut session);
    press(&mut session, "Your name");
    type_text(&mut session, "   ");
    press(&mut session, "Close");
    wait_for(
        &mut session,
        "the blank field to change nothing",
        |session| (name_of(session) == "Ada").then_some(()),
    );
}
