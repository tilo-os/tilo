//! A person names themselves in their own card, then renames themselves again: the field asks for a
//! name nobody has given yet, and what they type last is the name they go by.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call"
)]

use std::time::Duration;

use tilo_compositor::{InputEvent, OutputConfig};
use tilo_session::{Options, Session};
use ui::accesskit::Role;

use crate::common::{WIDTH, shot};

const HEIGHT: i32 = 800;
/// The Backspace key, as the compositor's keyboard reads it.
const KEY_BACKSPACE: u32 = 14;
/// The Escape key.
const KEY_ESCAPE: u32 = 1;

/// The name the card's field holds now, as assistive technology reads it.
fn field_value(session: &Session) -> Option<String> {
    for (_, _, tree) in session.a11y_windows() {
        for (_, node) in tree.nodes {
            if node.role() == Role::TextInput
                && let Some(value) = node.value()
            {
                return Some(value.to_owned());
            }
        }
    }
    None
}

/// Step until `done` holds, or fail saying what the shell showed.
fn until(session: &mut Session, what: &str, mut done: impl FnMut(&Session) -> bool) {
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    loop {
        if done(session) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "waiting for {what}: {:?}",
            session.accessible_labels()
        );
        session.step(Duration::from_millis(16)).expect("step");
    }
}

/// Where the control named `name` stands.
fn control_named(session: &Session, name: &str) -> Option<(f64, f64, f64, f64)> {
    session
        .controls()
        .into_iter()
        .find(|control| control.name == name)
        .map(|control| control.rect)
}

/// Press the screen point at the centre of `rect`.
fn press_rect(session: &mut Session, (x, y, w, h): (f64, f64, f64, f64)) {
    session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: x + w / 2.0,
            y: y + h / 2.0,
        })
        .expect("the pointer");
    // The pointer comes to rest over the control first, as a person's does before pressing it.
    session.step(Duration::from_millis(16)).expect("step");
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: 0x110,
                pressed,
            })
            .expect("the button");
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// Press the key `keycode`.
fn key(session: &mut Session, keycode: u32) {
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::Key { keycode, pressed })
            .expect("the key");
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// Type `text`, a key at a time.
fn type_text(session: &mut Session, text: &str) {
    for letter in text.chars() {
        assert!(
            session.compositor().type_char(letter).expect("a key"),
            "no key types {letter:?}"
        );
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// Where the bar's own avatar stands, once the bar shows it.
fn own_avatar(session: &mut Session) -> (f64, f64, f64, f64) {
    let mut avatar = None;
    until(session, "the bar's own avatar", |session| {
        avatar = session
            .controls()
            .into_iter()
            .find(|control| control.name.ends_with("(you): your profile"))
            .map(|control| control.rect);
        avatar.is_some()
    });
    avatar.expect("the bar's own avatar")
}

/// Open the person's own card from the bar's own avatar: the pointer comes up to the bar and rests
/// there while the shell settles, as a person's does, and presses the avatar where it then stands.
fn open_own_card(session: &mut Session) {
    let (x, y, w, h) = own_avatar(session);
    session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: x + w / 2.0,
            y: y + h / 2.0,
        })
        .expect("the pointer");
    for _ in 0..30 {
        session.step(Duration::from_millis(16)).expect("step");
    }
    let avatar = own_avatar(session);
    press_rect(session, avatar);
    until(session, "the card's name field", |session| {
        control_named(session, "Your name").is_some()
    });
}

/// Type `name` into the card's name field and close the card, as a person does: the field cleared,
/// the name typed, and the card closed with what was typed kept.
fn rename_to(session: &mut Session, name: &str) {
    let field = control_named(session, "Your name").expect("the name field");
    press_rect(session, field);
    for _ in 0..64 {
        key(session, KEY_BACKSPACE);
    }
    type_text(session, name);
    key(session, KEY_ESCAPE);
    until(session, "the card closed", |session| {
        control_named(session, "Your name").is_none()
    });
}

/// The person's identity as this device keeps it: what provisioning wrote, whose name is nobody's
/// yet, so the card has a name to ask for.
fn make_the_identity_unnamed(home: &tilo_test_env::TestHome) {
    let dir = home.config().join("tilo").join("identity");
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

#[test]
fn the_name_field_asks_for_a_name_and_keeps_the_last_one_given() {
    let _settled = ui::motion::testing::settled();
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
            ..Options::default()
        },
    );
    open_own_card(&mut session);
    assert_eq!(
        field_value(&session).unwrap_or_default(),
        "",
        "a field asking for a name holds none: the words for a person without one are its hint"
    );
    shot(&mut session, "name-field-empty");

    rename_to(&mut session, "Grace");
    open_own_card(&mut session);
    assert_eq!(
        field_value(&session).as_deref(),
        Some("Grace"),
        "the name they gave is in the field"
    );

    rename_to(&mut session, "Grace Hopper");
    open_own_card(&mut session);
    assert_eq!(
        field_value(&session).as_deref(),
        Some("Grace Hopper"),
        "a name changed again is kept again, never the first one back"
    );
    shot(&mut session, "name-field-renamed");
}

#[test]
fn a_notification_does_not_take_a_press_on_the_name_field() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let state = crate::common::device_in(&mut home);
    make_the_identity_unnamed(&home);
    tilo_session::identity::install();
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            state: Some(state),
            ..Options::default()
        },
    );
    let title = "You joined Garden club";
    session.notifications().post(
        tilo_session_core::desktop::notifications::Notification::new(title, "")
            .with_dismiss_after(Duration::from_secs(60)),
    );
    until(&mut session, "the banner on screen", |session| {
        session.a11y_windows().into_iter().any(|(_, _, tree)| {
            tree.nodes
                .into_iter()
                .any(|(_, node)| node.value() == Some(title))
        })
    });

    open_own_card(&mut session);
    let field = control_named(&session, "Your name").expect("the name field over the banner");
    press_rect(&mut session, field);
    type_text(&mut session, "Lin");
    assert_eq!(field_value(&session).as_deref(), Some("Lin"));
    shot(&mut session, "name-field-in-front-of-notification");
}
