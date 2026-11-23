//! The on-screen keyboard as the session keeps it: an app of its own, started
//! while the person may want it, on the display they work on, and up
//! while a text field waits for typing on the glass.
//!
//! These start the keyboard the way the session does — through its own surface
//! machinery, over an app wire only this test reaches — and touch a window's text
//! field through the compositor: what a person with the machine in their hands
//! does. What the keyboard does is read from the session's published resource,
//! which is what its own app reads, and from what the displays show.

use super::*;

use std::path::{Path, PathBuf};
use std::time::Instant;

use tilo_accessibility_settings::OnScreenKeyboard;
use tilo_session_core::desktop::verbs::SHELL_RESOURCE;
use tilo_session::AppSocket;
use tilo_session::keyboard;

use super::lock::{CLOSE, apart, pictures, private_home};

/// The resource the session publishes the keyboard as.
const KEYBOARD: &str = "keyboard";

/// The displays these tests work across: the one the person starts on, and one beside it.
const FIRST: &str = "HEADLESS-1";
const SECOND: &str = "HEADLESS-2";

/// One display's refresh, while waiting for the keyboard to draw.
const REFRESH: Duration = Duration::from_millis(16);

/// The on-screen keyboard's app: an app of its own package, which this run builds the first time a
/// test brings the keyboard up.
fn keyboard_binary() -> PathBuf {
    tilo_test_env::sibling_binary("osk", "osk")
}

/// A session on two displays that starts its on-screen keyboard as the person's choice says,
/// serving its services on `socket` .
fn keyboard_session(socket: &Path, choice: OnScreenKeyboard) -> Session {
    crate::common::headless_on(
        &[
            OutputConfig::new(FIRST, WIDTH, HEIGHT),
            OutputConfig::new(SECOND, 1024, 768).at(WIDTH, 0),
        ],
        Options {
            user: "ada".to_owned(),
            collab: false,
            app_socket: AppSocket::At(socket.to_path_buf()),
            surfaces: vec![keyboard::surface_of(keyboard_binary())],
            config: tilo_session::SessionConfig {
                on_screen_keyboard: choice,
                ..tilo_session::SessionConfig::default()
            },
            ..Options::default()
        },
    )
}

/// The middle of `rect` : where a finger landing on a window touches it.
fn middle(rect: (i32, i32, i32, i32)) -> (f64, f64) {
    (
        f64::from(rect.0) + f64::from(rect.2) / 2.0,
        f64::from(rect.1) + f64::from(rect.3) / 2.0,
    )
}

/// A point inside `rect` 's left quarter: where a finger lands on a window that another window
/// stands beside.
fn clear_of_the_next(rect: (i32, i32, i32, i32)) -> (f64, f64) {
    (
        f64::from(rect.0) + f64::from(rect.2) * 0.25,
        f64::from(rect.1) + f64::from(rect.3) / 2.0,
    )
}

/// A finger touching `display` at (`x`, `y` ) and lifting again: a tap on the glass, as a person
/// gives one.
pub(super) fn finger(session: &mut Session, (x, y): (f64, f64)) {
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

/// Step the session until `displays` are not changing any more, and answer what they show then: the
/// shell as it stands, or the keyboard as it stands.
#[track_caller]
fn still(session: &mut Session, displays: &[&str]) -> Vec<Vec<f64>> {
    let deadline = tilo_test_env::hang_deadline();
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

/// Step the session until `display` shows something other than `was` , settled: the keyboard, drawn
/// there.
#[track_caller]
fn until_keyboard(session: &mut Session, display: &str, was: &[f64]) -> Vec<f64> {
    let deadline = tilo_test_env::hang_deadline();
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

/// Step the session until `display` shows the shell it showed before the keyboard was up again, and
/// is not changing any more.
#[track_caller]
fn until_shell(session: &mut Session, display: &str, was: &[f64]) -> Vec<f64> {
    let deadline = tilo_test_env::hang_deadline();
    let mut settled: Option<Vec<f64>> = None;
    loop {
        let now = pictures(session, &[display]).remove(0);
        if apart(&now, was) < CLOSE {
            if settled.as_ref() == Some(&now) {
                return now;
            }
            settled = Some(now);
        } else {
            settled = None;
        }
        assert!(
            Instant::now() < deadline,
            "the shell never came back on {display}"
        );
        session.step(REFRESH).expect("a turn of the session");
    }
}

/// The place `display` stands in, as the session lays its displays out.
#[track_caller]
fn display_rect(session: &mut Session, display: &str) -> (i32, i32, i32, i32) {
    session
        .compositor()
        .outputs()
        .into_iter()
        .find(|output| output.name == display)
        .and_then(|output| output.rect)
        .unwrap_or_else(|| panic!("the session shows {display}"))
}

/// `app` 's window, once it stops moving: where the person sees it.
#[track_caller]
fn resting_window(session: &mut Session, app: &str) -> (u64, (i32, i32, i32, i32)) {
    let deadline = tilo_test_env::hang_deadline();
    let mut settled = None;
    loop {
        let now = window(session.compositor(), app).expect("the window");
        if settled == Some(now) {
            return now;
        }
        settled = Some(now);
        assert!(Instant::now() < deadline, "{app} never came to rest");
        session.step(REFRESH).expect("a turn of the session");
    }
}

/// Move the focused window to the display beside the one it stands on, and answer the display it
/// stands on then.
#[track_caller]
fn move_beside(session: &mut Session, app: &str) -> String {
    let (_, before) = window(session.compositor(), app).expect("the window");
    let was = display_of(session, before);
    let here = display_rect(session, &was);
    // The shell steps rightwards, or leftwards from the rightmost display.
    let another_rightwards = session
        .compositor()
        .outputs()
        .into_iter()
        .filter(|output| output.enabled)
        .filter_map(|output| output.rect)
        .any(|(x, _, _, _)| x > here.0);
    let steps = if another_rightwards { 1 } else { -1 };
    invoke_with(
        session,
        SHELL_RESOURCE,
        "move-to-display",
        &serde_json::json!({ "steps": steps }),
    );
    pump(session, &mut [], |session, _| {
        window(session.compositor(), app).is_some_and(|(_, rect)| display_of(session, rect) != was)
    });
    let (_, rect) = window(session.compositor(), app).expect("the window");
    display_of(session, rect)
}

/// Ask into `client`'s field until the session has the asking: the keyboard
/// its app runs is read from what the session publishes. The field asks again
/// while the session has not taken it, as a toolkit asks again when its window
/// takes the focus — the keyboard's input method connects in its own process,
/// seconds after its window.
#[track_caller]
fn type_until_the_keyboard_is_up(session: &mut Session, client: &mut TestClient) {
    let deadline = tilo_test_env::hang_deadline();
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
            "the keyboard never came up for a field: {} wants_typing {} focused {:?} windows {:?}",
            reading(session),
            session.compositor().text_entry().wants_typing,
            session
                .compositor()
                .focused_window()
                .map(|focused| focused.id),
            session
                .compositor()
                .windows()
                .iter()
                .map(|(window, rect)| (window.id, *rect))
                .collect::<Vec<_>>()
        );
    }
}

/// `session` 's reading of the keyboard.
fn reading(session: &Session) -> serde_json::Value {
    session.read(KEYBOARD).unwrap_or_else(|| {
        panic!(
            "the session publishes its keyboard: {:?}",
            session.surfaces().names()
        )
    })
}

/// The display `rect` stands on, as the session places its displays: the one the person sees the
/// window on.
#[track_caller]
fn display_of(session: &mut Session, rect: (i32, i32, i32, i32)) -> String {
    let (x, y, w, h) = rect;
    let cx = i64::from(x).saturating_add(i64::from(w).saturating_div(2));
    let cy = i64::from(y).saturating_add(i64::from(h).saturating_div(2));
    session
        .compositor()
        .outputs()
        .into_iter()
        .find(|output| {
            output.enabled
                && output.rect.is_some_and(|(x, y, w, h)| {
                    let (x, y) = (i64::from(x), i64::from(y));
                    cx >= x
                        && cx < x.saturating_add(i64::from(w))
                        && cy >= y
                        && cy < y.saturating_add(i64::from(h))
                })
        })
        .unwrap_or_else(|| panic!("a display stands under the window at {rect:?}"))
        .name
}

/// The displays the session shows on, in its own order.
fn displays(session: &mut Session) -> Vec<String> {
    session
        .compositor()
        .outputs()
        .into_iter()
        .filter(|output| output.enabled && output.rect.is_some())
        .map(|output| output.name)
        .collect()
}

/// A touch on the display the person works on, then a text field in the window they touched: the
/// keyboard comes up there, and only there.
#[test]
fn a_touch_into_a_field_brings_the_keyboard_up_on_that_display() {
    const APP: &str = "org.example.Notes";
    let _settled = ui::motion::testing::settled();
    let home = private_home();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_RUNTIME_DIR", home.path());
    let mut session = keyboard_session(&home.path().join("collab.sock"), OnScreenKeyboard::Auto);
    let mut client = mapped(&mut session, APP);
    let (_, opened) = resting_window(&mut session, APP);
    let opened_on = display_of(&mut session, opened);
    // The person works on the other display, under the hands they type with: their window goes with
    // them.
    let typing_on = move_beside(&mut session, APP);
    let (_, rect) = resting_window(&mut session, APP);
    assert_ne!(
        typing_on, opened_on,
        "the window moved to the display beside it: {rect:?}"
    );
    let left_alone = displays(&mut session)
        .into_iter()
        .find(|display| *display != typing_on)
        .expect("another display beside the one the person works on");

    // Nothing has been touched yet: a shell typed on by a keyboard and a pointer needs no keyboard
    // of its own.
    for _ in 0..8 {
        session.step(REFRESH).expect("a turn of the session");
    }
    assert!(
        !session.surfaces().running(keyboard::SURFACE),
        "no keyboard starts for a shell that was not touched: {:?}",
        session.surfaces().names()
    );

    // A finger on that window, and into its field: the machine in the person's hands gets a
    // keyboard.
    finger(&mut session, middle(rect));
    pump(&mut session, &mut [&mut client], |session, _| {
        session.surfaces().running(keyboard::SURFACE)
    });
    assert!(
        session.surfaces().running(keyboard::SURFACE),
        "the keyboard's app runs once the glass is typed on: {:?}",
        session.surfaces().names()
    );
    // It stands there with no field to serve: nothing is drawn over the shell.
    let shell = still(&mut session, &[&typing_on, &left_alone]);
    assert_eq!(
        reading(&session)["visible"],
        false,
        "no field has asked for typing yet"
    );

    // The field asks. The keyboard's input method connects in its own process, so a field that
    // asked before it was there asks again, as a toolkit asks again when its window takes the
    // focus.
    type_until_the_keyboard_is_up(&mut session, &mut client);
    assert_eq!(
        reading(&session)["display"],
        typing_on,
        "the keyboard stands on the display the person types on: {}",
        reading(&session)
    );
    let shown = until_keyboard(&mut session, &typing_on, &shell[0]);
    let drawn = apart(&shown, &shell[0]);
    assert!(
        drawn > CLOSE,
        "the keyboard draws over the display the person types on: {drawn:.0} of 255 from the shell \
         under it"
    );
    let elsewhere = pictures(&mut session, &[&left_alone]).remove(0);
    let untouched = apart(&elsewhere, &shell[1]);
    assert!(
        untouched < CLOSE,
        "the display the person is not typing on keeps its shell: {untouched:.0} of 255"
    );
}

/// The always-on choice: the keyboard stands on a display with no field and nothing touched at all.
#[test]
fn an_always_on_keyboard_is_up_where_the_person_works_with_no_field_at_all() {
    let _settled = ui::motion::testing::settled();
    let home = private_home();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_RUNTIME_DIR", home.path());
    let mut session = keyboard_session(&home.path().join("collab.sock"), OnScreenKeyboard::Always);
    let displays = displays(&mut session);
    assert_eq!(displays.len(), 2, "two displays: {displays:?}");
    // The shell as it stands with no keyboard drawn over it: starting the keyboard's app takes a
    // whole app's launch, seconds after the shell.
    let shell = still(&mut session, &[&displays[0], &displays[1]]);

    pump(&mut session, &mut [], |session, _| {
        session.surfaces().running(keyboard::SURFACE) && reading(session)["visible"] == true
    });
    let stands = reading(&session)["display"]
        .as_str()
        .expect("the display the keyboard stands on")
        .to_owned();
    let at = displays
        .iter()
        .position(|display| *display == stands)
        .unwrap_or_else(|| panic!("the keyboard stands on one of {displays:?}: {stands}"));
    let shown = until_keyboard(&mut session, &stands, &shell[at]);
    let drawn = apart(&shown, &shell[at]);
    assert!(
        drawn > CLOSE,
        "the keyboard draws with no field to serve: {drawn:.0} of 255 from the shell under it"
    );
    let beside = usize::from(at == 0);
    let elsewhere = pictures(&mut session, &[&displays[beside]]).remove(0);
    let untouched = apart(&elsewhere, &shell[beside]);
    assert!(
        untouched < CLOSE,
        "it stands on one display only: {untouched:.0} of 255 on {}",
        displays[beside]
    );
}

/// A field that loses the focus takes the keyboard with it: another window of the person's takes
/// the focus, and the keyboard goes away.
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
    let (_, opened) = resting_window(&mut session, NOTES);
    let opened_on = display_of(&mut session, opened);
    // The person works on the display beside it: their window goes with them.
    let working_on = move_beside(&mut session, NOTES);
    assert_ne!(
        working_on, opened_on,
        "the window moved to the display beside it"
    );
    let notes_rect = resting_window(&mut session, NOTES).1;
    // Another window of theirs opens on the display they work on.
    let mut mail = mapped(&mut session, MAIL);
    let mail_rect = resting_window(&mut session, MAIL).1;
    assert_eq!(
        display_of(&mut session, mail_rect),
        working_on,
        "a window opens on the display the person works on: {mail_rect:?}"
    );

    // The person touches the window without a field: the keyboard's app starts and keeps still.
    finger(&mut session, middle(mail_rect));
    pump(&mut session, &mut [&mut notes, &mut mail], |session, _| {
        session.surfaces().running(keyboard::SURFACE)
    });
    let shell = still(&mut session, &[&working_on]);
    assert_eq!(
        reading(&session)["visible"],
        false,
        "nothing has asked for typing"
    );

    // Into the other window's field, on the side the person can still reach now that the second
    // window stands beside it: the keyboard comes up.
    finger(&mut session, clear_of_the_next(notes_rect));
    type_until_the_keyboard_is_up(&mut session, &mut notes);
    let shown = until_keyboard(&mut session, &working_on, &shell[0]);
    let drawn = apart(&shown, &shell[0]);
    assert!(
        drawn > CLOSE,
        "the keyboard draws over the display: {drawn:.0} of 255 from the shell under it"
    );

    // The person turns to their other window: the field is left, and the keyboard goes away with
    // it.
    let mail_tab = window_resource(&session, MAIL).expect("the shell publishes the window");
    invoke_with(
        &mut session,
        mail_tab.id.as_str(),
        "focus",
        &serde_json::json!({}),
    );
    pump(&mut session, &mut [&mut notes, &mut mail], |session, _| {
        reading(session)["visible"] == false
    });
    assert_eq!(
        session
            .compositor()
            .focused_window()
            .and_then(|focused| focused.app_id)
            .as_deref(),
        Some(MAIL),
        "the other window has the focus now"
    );
    let back = until_shell(&mut session, &working_on, &shell[0]);
    let rest = apart(&back, &shell[0]);
    assert!(
        rest < CLOSE,
        "the keyboard is gone from the display the person left it on: {rest:.0} of 255 from the \
         shell under it"
    );
    assert!(
        !session.compositor().text_entry().wants_typing,
        "no field has the focus any more"
    );
}

/// The person puts the keyboard away, and an agent asks for it back: both are the keyboard's own
/// published action, so what a press of its Dismiss asks for is what a step of an agent's asks for.
#[test]
fn the_keyboard_shows_and_hides_by_its_published_action() {
    let _settled = ui::motion::testing::settled();
    let home = private_home();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_RUNTIME_DIR", home.path());
    let mut session = keyboard_session(&home.path().join("collab.sock"), OnScreenKeyboard::Always);
    pump(&mut session, &mut [], |session, _| {
        session.surfaces().running(keyboard::SURFACE) && reading(session)["visible"] == true
    });

    // Away, as the keyboard's own Dismiss asks: the session stops saying it shows, which no
    // reading of its own would have taken back.
    session
        .invoke_with_actor(
            &crate::common::as_person("ada"),
            KEYBOARD,
            "set-visible",
            &serde_json::json!({ "visible": false }),
        )
        .expect("the keyboard is put away");
    pump(&mut session, &mut [], |session, _| {
        reading(session)["visible"] == false
    });
    assert!(
        session.surfaces().running(keyboard::SURFACE),
        "its app keeps running: it is back on the next touch of a field"
    );

    // And back, as any step of an agent's that asks for it does.
    session
        .invoke_with_actor(
            &crate::common::agent("helper"),
            KEYBOARD,
            "set-visible",
            &serde_json::json!({ "visible": true }),
        )
        .expect("the keyboard is asked back");
    pump(&mut session, &mut [], |session, _| {
        reading(session)["visible"] == true
    });
}

/// A control of the session's API by name, and where it stands.
type Control = (String, (f64, f64, f64, f64));

/// The keys of the keyboard on `display`, each with where it stands: the single-letter controls of
/// the session's API in the lower half of the display, where the keyboard stands.
fn keys_on(session: &Session, display: (i32, i32, i32, i32)) -> Vec<Control> {
    let low = f64::from(display.1) + f64::from(display.3) * 0.5;
    controls(session)
        .into_iter()
        .filter(|(name, rect)| {
            name.chars().count() == 1 && name.chars().all(char::is_alphabetic) && rect.1 >= low
        })
        .collect()
}

/// Every control of the session's API by name and place: what a person's tools, an agent and a
/// test find and press.
fn controls(session: &Session) -> Vec<Control> {
    session
        .controls()
        .into_iter()
        .map(|control| (control.name, control.rect))
        .collect()
}

/// The keyboard's keys are controls of a window like any other's: the session's API lists them
/// where the person sees them, and a press of one through it types into the field the keyboard
/// serves, as a tap on it does. The desktop's own controls stand as they did beside it.
#[test]
fn the_keyboards_keys_are_listed_and_pressed_through_the_sessions_api() {
    const APP: &str = "org.example.Notes";
    let _settled = ui::motion::testing::settled();
    let home = private_home();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_RUNTIME_DIR", home.path());
    let mut session = keyboard_session(&home.path().join("collab.sock"), OnScreenKeyboard::Auto);
    let mut client = mapped(&mut session, APP);
    let (_, rect) = resting_window(&mut session, APP);
    let on = display_of(&mut session, rect);
    let display = display_rect(&mut session, &on);
    assert!(
        keys_on(&session, display).is_empty(),
        "no keyboard, no keys: {:?}",
        controls(&session)
    );

    // A finger into the window's field: the keyboard comes up for it.
    finger(&mut session, middle(rect));
    pump(&mut session, &mut [&mut client], |session, _| {
        session.surfaces().running(keyboard::SURFACE)
    });
    type_until_the_keyboard_is_up(&mut session, &mut client);
    pump(&mut session, &mut [&mut client], |session, _| {
        !keys_on(session, display).is_empty()
    });
    let keys = keys_on(&session, display);
    let namespace = tilo_session::surfaces::namespace(keyboard::SURFACE);
    let (kx, ky, kw, kh) = session
        .compositor()
        .layer_rect(&namespace)
        .expect("the keyboard stands on the display");
    let (left, top) = (f64::from(kx), f64::from(ky));
    let (right, bottom) = (f64::from(kx + kw) + 0.5, f64::from(ky + kh) + 0.5);
    for (name, (x, y, w, h)) in &keys {
        assert!(
            *x >= left && *y >= top && x + w <= right && y + h <= bottom,
            "key {name} at {:?} stands where the keyboard is drawn, {:?}",
            (x, y, w, h),
            (kx, ky, kw, kh)
        );
    }
    assert!(
        keys.len() >= 20,
        "the keyboard's letters are listed, each as a control: {keys:?}"
    );

    // A press of a key through the API types it into the field, as the keyboard types a tap.
    let (letter, _) = keys
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("q"))
        .or_else(|| keys.first())
        .cloned()
        .expect("a key");
    session
        .press_action(&letter, ui::window_targets::TargetAction::Activate)
        .unwrap_or_else(|error| panic!("the key {letter} is pressed: {error}"));
    pump(&mut session, &mut [&mut client], |_, clients| {
        let seen = &clients[0].seen;
        format!("{}{}", seen.committed, seen.composed).eq_ignore_ascii_case(&letter)
    });

    // Beside the keyboard the desktop's controls stand as they did: put away, the keyboard takes
    // its keys with it and leaves the rest where it found them.
    let up = controls(&session);
    session
        .invoke_with_actor(
            &crate::common::as_person("ada"),
            KEYBOARD,
            "set-visible",
            &serde_json::json!({ "visible": false }),
        )
        .expect("the keyboard is put away");
    pump(&mut session, &mut [&mut client], |session, _| {
        keys_on(session, display).is_empty()
    });
    let away = controls(&session);
    for control in &away {
        assert!(
            up.contains(control),
            "the desktop's control {control:?} stood as it does while the keyboard was up: {up:?}"
        );
    }
    let gone: Vec<&Control> = up
        .iter()
        .filter(|control| !away.contains(control))
        .collect();
    assert!(
        gone.iter()
            .all(|(_, (x, y, w, h))| *x >= left && *y >= top && x + w <= right && y + h <= bottom),
        "only the keyboard's own controls went with it: {gone:?}"
    );
}
