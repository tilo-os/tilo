//! The on-screen keyboard as the session keeps it: an app of its own, started
//! while the person may want it, standing on the display they work on, and up
//! while a text field waits for typing on the glass.
//!
//! These start the keyboard the way the session does — through its own surface
//! machinery, over a plane only this test reaches — and touch a window's text
//! field through the compositor: what a person with the machine in their hands
//! does. What the keyboard does is read from the session's published resource,
//! which is what its own app reads, and from what the displays show.

use super::*;

use std::path::{Path, PathBuf};
use std::time::Instant;

use tilo_accessibility_settings::OnScreenKeyboard;
use tilo_desktop::desktop_actions::DESK_RESOURCE;
use tilo_session::ActionPlane;
use tilo_session::keyboard;

use super::lock::{CLOSE, apart, pictures, private_home};

/// The resource the session publishes the keyboard as.
const KEYBOARD: &str = "keyboard";

/// The displays these tests work across: the one the person starts on, and one
/// beside it.
const FIRST: &str = "HEADLESS-1";
const SECOND: &str = "HEADLESS-2";

/// How long the keyboard's app may take to come up and draw: a whole app
/// starts here.
const OPENING: Duration = Duration::from_secs(90);

/// One display's refresh, while waiting for the keyboard to draw.
const REFRESH: Duration = Duration::from_millis(16);

/// The on-screen keyboard's app, built beside the session's own.
fn keyboard_binary() -> PathBuf {
    let binary = tilo_test_env::package_binary!("tilo-session").with_file_name("osk");
    assert!(
        binary.exists(),
        "expected the on-screen keyboard at {}; build the workspace first",
        binary.display()
    );
    binary
}

/// A session on two displays that starts its on-screen keyboard as the
/// person's choice says, serving its services on `socket`.
fn keyboard_session(socket: &Path, choice: OnScreenKeyboard) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            user: "ada".to_owned(),
            collab: false,
            action_plane: ActionPlane::At(socket.to_path_buf()),
            surfaces: vec![keyboard::surface_of(keyboard_binary())],
            config: tilo_session::SessionConfig {
                on_screen_keyboard: choice,
                ..tilo_session::SessionConfig::default()
            },
            ..Options::default()
        },
        &[
            OutputConfig::new(FIRST, WIDTH, HEIGHT),
            OutputConfig::new(SECOND, 1024, 768).at(WIDTH, 0),
        ],
    )
    .expect("session")
}

/// The middle of `rect`: where a finger landing on a window touches it.
fn middle(rect: (i32, i32, i32, i32)) -> (f64, f64) {
    (
        f64::from(rect.0) + f64::from(rect.2) / 2.0,
        f64::from(rect.1) + f64::from(rect.3) / 2.0,
    )
}

/// A finger touching `display` at (`x`, `y`) and lifting again: a tap on the
/// glass, as a person gives one.
fn finger(session: &mut Session, (x, y): (f64, f64)) {
    for event in [
        InputEvent::TouchDown { slot: 0, x, y },
        InputEvent::TouchFrame,
        InputEvent::TouchUp { slot: 0 },
        InputEvent::TouchFrame,
    ] {
        session.compositor().input(event).expect("a touch");
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// Step the session until `displays` are not changing any more, and answer
/// what they show then: the desk as it stands, or the keyboard as it stands.
#[track_caller]
fn still(session: &mut Session, displays: &[&str]) -> Vec<Vec<f64>> {
    let deadline = Instant::now() + OPENING;
    let mut settled: Option<Vec<Vec<f64>>> = None;
    loop {
        let now = pictures(session, displays);
        if settled.as_ref() == Some(&now) {
            return now;
        }
        settled = Some(now);
        assert!(
            Instant::now() < deadline,
            "{displays:?} never stopped changing"
        );
        session.step(REFRESH).expect("a turn of the session");
    }
}

/// Step the session until `display` shows something other than `was`, settled:
/// the keyboard, drawn there.
#[track_caller]
fn until_keyboard(session: &mut Session, display: &str, was: &[f64]) -> Vec<f64> {
    let deadline = Instant::now() + OPENING;
    let mut settled: Option<Vec<f64>> = None;
    loop {
        let now = pictures(session, &[display]).remove(0);
        if apart(&now, was) > CLOSE {
            if settled.as_ref() == Some(&now) {
                return now;
            }
            settled = Some(now);
        } else {
            settled = None;
        }
        assert!(
            Instant::now() < deadline,
            "the keyboard never drew on {display}"
        );
        session.step(REFRESH).expect("a turn of the session");
    }
}

/// Ask into `client`'s field until the session has the asking: the keyboard
/// its app runs is read from what the session publishes. The field asks again
/// while the session has not taken it, as a toolkit asks again when its window
/// takes the focus — the keyboard's input method connects in its own process,
/// seconds after its window.
#[track_caller]
fn type_until_the_keyboard_is_up(session: &mut Session, client: &mut TestClient) {
    let deadline = Instant::now() + OPENING;
    loop {
        if !session.compositor().text_entry().wants_typing {
            client.type_into_a_field();
        }
        client.exchange();
        session.step(REFRESH).expect("a turn of the session");
        client.exchange();
        if reading(session)["visible"] == true {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the keyboard never came up for a field: {}",
            reading(session)
        );
    }
}

/// `session`'s reading of the keyboard.
fn reading(session: &Session) -> serde_json::Value {
    session.read(KEYBOARD).unwrap_or_else(|| {
        panic!(
            "the session publishes its keyboard: {:?}",
            session.surfaces().names()
        )
    })
}

/// A touch on the display the person works on, then a text field in the window
/// they touched: the keyboard comes up there, and only there.
#[test]
fn a_touch_into_a_field_brings_the_keyboard_up_on_that_display() {
    const APP: &str = "org.example.Notes";
    let _settled = ui::motion::testing::settled();
    let home = private_home();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_RUNTIME_DIR", home.path());
    let mut session = keyboard_session(&home.path().join("collab.sock"), OnScreenKeyboard::Auto);
    let mut client = mapped(&mut session, APP);
    // The person works on the second display, under the hands they type with:
    // their window goes with them.
    invoke_with(
        &mut session,
        DESK_RESOURCE,
        "move-to-display",
        &serde_json::json!({ "steps": 1 }),
    );
    let (_, rect) = window(session.compositor(), APP).expect("the window");
    assert!(
        rect.0 >= WIDTH,
        "the window moved to the second display: {rect:?}"
    );

    // A finger on that window, and into its field: nothing starts a keyboard
    // for a desktop typed on by a keyboard and a pointer, and a machine that
    // was touched gets one.
    finger(&mut session, middle(rect));
    pump(&mut session, &mut [&mut client], |session, _| {
        session.surfaces().running(keyboard::SURFACE)
    });
    assert!(
        session.surfaces().running(keyboard::SURFACE),
        "the keyboard's app runs once the glass is typed on: {:?}",
        session.surfaces().names()
    );
    // It stands there with no field to serve: nothing is drawn over the desk.
    let desk = still(&mut session, &[FIRST, SECOND]);
    assert_eq!(
        reading(&session)["visible"],
        false,
        "no field has asked for typing yet"
    );

    // The field asks. The keyboard's input method connects in its own
    // process, so a field that asked before it was there asks again, as a
    // toolkit asks again when its window takes the focus.
    type_until_the_keyboard_is_up(&mut session, &mut client);
    assert_eq!(
        reading(&session)["display"],
        SECOND,
        "the keyboard stands on the display the person types on: {}",
        reading(&session)
    );

    let shown = until_keyboard(&mut session, SECOND, &desk[1]);
    let drawn = apart(&shown, &desk[1]);
    assert!(
        drawn > CLOSE,
        "the keyboard draws over the display the person types on: {drawn:.0} of 255 from the desk \
         under it"
    );
    let elsewhere = pictures(&mut session, &[FIRST]).remove(0);
    let untouched = apart(&elsewhere, &desk[0]);
    assert!(
        untouched < CLOSE,
        "the display the person is not typing on keeps its desk: {untouched:.0} of 255"
    );
}

/// The always-on choice: the keyboard stands on the person's display with no
/// field and nothing touched at all.
#[test]
fn an_always_on_keyboard_is_up_where_the_person_works_with_no_field_at_all() {
    let _settled = ui::motion::testing::settled();
    let home = private_home();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_RUNTIME_DIR", home.path());
    let mut session = keyboard_session(&home.path().join("collab.sock"), OnScreenKeyboard::Always);
    // The desk as it stands with no keyboard drawn over it: starting the
    // keyboard's app takes a whole app's launch, seconds after the desk.
    let desk = still(&mut session, &[FIRST, SECOND]);

    pump(&mut session, &mut [], |session, _| {
        session.surfaces().running(keyboard::SURFACE) && reading(session)["visible"] == true
    });
    assert_eq!(
        reading(&session)["display"],
        FIRST,
        "it stands on the display there is: {}",
        reading(&session)
    );
    let shown = until_keyboard(&mut session, FIRST, &desk[0]);
    let drawn = apart(&shown, &desk[0]);
    assert!(
        drawn > CLOSE,
        "the keyboard draws with no field to serve: {drawn:.0} of 255 from the desk under it"
    );
    let elsewhere = pictures(&mut session, &[SECOND]).remove(0);
    let untouched = apart(&elsewhere, &desk[1]);
    assert!(
        untouched < CLOSE,
        "it stands on one display only: {untouched:.0} of 255"
    );
}

/// A field that loses the focus takes the keyboard with it: another window of
/// the person's takes the focus, and the keyboard goes away.
#[test]
fn the_keyboard_goes_away_when_the_field_loses_the_focus() {
    const NOTES: &str = "org.example.Notes";
    const MAIL: &str = "org.example.Mail";
    let _settled = ui::motion::testing::settled();
    let home = private_home();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_RUNTIME_DIR", home.path());
    let mut session = keyboard_session(&home.path().join("collab.sock"), OnScreenKeyboard::Auto);
    let mut notes = mapped(&mut session, NOTES);
    invoke_with(
        &mut session,
        DESK_RESOURCE,
        "move-to-display",
        &serde_json::json!({ "steps": 1 }),
    );
    let mut mail = mapped(&mut session, MAIL);
    let (_, mail_rect) = window(session.compositor(), MAIL).expect("the second window");
    assert!(
        mail_rect.0 >= WIDTH,
        "a window opens on the display the person works on: {mail_rect:?}"
    );

    // The person touches the window without a field: the keyboard's app starts
    // and keeps still.
    finger(&mut session, middle(mail_rect));
    pump(&mut session, &mut [&mut notes, &mut mail], |session, _| {
        session.surfaces().running(keyboard::SURFACE)
    });
    let desk = still(&mut session, &[SECOND]);
    assert_eq!(
        reading(&session)["visible"],
        false,
        "nothing has asked for typing"
    );

    // Into the other window's field: the keyboard comes up over it.
    let notes_rect = window(session.compositor(), NOTES)
        .expect("the first window")
        .1;
    finger(&mut session, middle(notes_rect));
    type_until_the_keyboard_is_up(&mut session, &mut notes);
    until_keyboard(&mut session, SECOND, &desk[0]);

    // The other window takes the focus: the field is left, and the keyboard
    // goes away with it.
    finger(&mut session, middle(mail_rect));
    pump(&mut session, &mut [&mut notes, &mut mail], |session, _| {
        reading(session)["visible"] == false
    });
    let back = still(&mut session, &[SECOND]);
    let rest = apart(&back[0], &desk[0]);
    assert!(
        rest < CLOSE,
        "the keyboard is gone from the display the person left it on: {rest:.0} of 255 from the \
         desk under it"
    );
    assert_eq!(
        session.compositor().text_entry().wants_typing,
        false,
        "no field has the focus any more"
    );
}
