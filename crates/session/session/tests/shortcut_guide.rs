//! The shortcut guide: F1 and the desktop's help chord put it up over the
//! display the person is working on, filled with the chords the session
//! answers now, and the same key, Escape or its Done take it down again.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::time::Duration;

use settings_store::hardware::{KeyChord, ShortcutAction};
use tilo_actions::ResourceSpec;
use tilo_compositor::{InputEvent, OutputConfig, Screenshot};
use tilo_desktop::desktop_actions::DISPLAY_KIND;
use tilo_session::{Options, Session, SessionConfig};
use tilo_sheets::Seat as _;
use tilo_sheets::shortcut_guide::Guide;
use ui::input::{KeyLegend, KeyboardBinding, KeyboardModifier};

const WIDTH: i32 = 1000;
const HEIGHT: i32 = 700;

/// What a key sends, evdev codes.
const KEY_ESCAPE: u32 = 1;
const KEY_LEFT_SHIFT: u32 = 42;
const KEY_F1: u32 = 59;
const KEY_SLASH: u32 = 53;
const KEY_LEFT_META: u32 = 125;

/// A session with the person's own settings.
fn session_with(config: SessionConfig, outputs: &[OutputConfig]) -> Session {
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            config,
            ..Options::default()
        },
        outputs,
    )
    .expect("session");
    session.step(Duration::from_millis(1)).expect("step");
    session
}

fn one_display() -> Session {
    session_with(
        SessionConfig::default(),
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
}

/// Two displays side by side.
fn docked() -> Session {
    session_with(
        SessionConfig::default(),
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT), {
            let mut right = OutputConfig::new("HEADLESS-2", WIDTH, HEIGHT);
            right.position = (WIDTH, 0);
            right
        }],
    )
}

/// Press or release `keycode` and let the session carry it out.
fn key(session: &mut Session, keycode: u32, pressed: bool) {
    session
        .compositor()
        .input(InputEvent::Key { keycode, pressed })
        .expect("the key");
    session.step(Duration::from_millis(1)).expect("step");
}

fn tap(session: &mut Session, keycode: u32) {
    key(session, keycode, true);
    key(session, keycode, false);
}

/// Press `keys` together, then let go of them in reverse.
fn chord(session: &mut Session, keys: &[u32]) {
    for keycode in keys {
        key(session, *keycode, true);
    }
    for keycode in keys.iter().rev() {
        key(session, *keycode, false);
    }
}

/// Step the session until `done` holds.
#[track_caller]
fn until(session: &mut Session, mut done: impl FnMut(&Session) -> bool) {
    for _ in 0..500 {
        session.step(Duration::from_millis(2)).expect("step");
        if done(session) {
            return;
        }
    }
    panic!("condition not reached");
}

/// What the guide shows now.
fn guide(session: &Session) -> Guide {
    session
        .shortcut_guide()
        .expect("the guide sheet mounted")
        .guide()
}

fn up(session: &Session) -> bool {
    session
        .shortcut_guide()
        .is_some_and(|seat| seat.presentation().is_open())
}

/// The displays the desk publishes, in reading order.
fn displays(session: &Session) -> Vec<ResourceSpec> {
    fn find(resources: &[ResourceSpec], hit: &dyn Fn(&ResourceSpec) -> bool) -> Vec<ResourceSpec> {
        resources
            .iter()
            .flat_map(|resource| {
                let mut found = find(&resource.children, hit);
                if hit(resource) {
                    found.insert(0, resource.clone());
                }
                found
            })
            .collect()
    }
    find(&session.resources(), &|resource| {
        resource.kind == DISPLAY_KIND
    })
}

/// Work on the display `index` of the desk, counting from the left, and wait
/// until the sheets follow it there.
fn focus(session: &mut Session, index: usize) {
    let display = displays(session)[index].id.clone();
    session
        .invoke(display.as_str(), "focus", &serde_json::Map::new())
        .unwrap_or_else(|error| panic!("{display:?} focus: {error:?}"));
    let left = i32::try_from(index).expect("a display") * WIDTH;
    let place = (
        left,
        0,
        u32::try_from(WIDTH).expect("a width"),
        u32::try_from(HEIGHT).expect("a height"),
    );
    until(session, |session| {
        session
            .shortcut_guide()
            .and_then(|seat| seat.presentation().place())
            == Some(place)
    });
}

/// A frame of `output`, once the session has drawn what it has.
fn shot(session: &mut Session, output: &str) -> Screenshot {
    session
        .compositor()
        .screenshot(output)
        .unwrap_or_else(|error| panic!("a frame of {output}: {error}"))
}

fn pixel(frame: &Screenshot, x: i32, y: i32) -> [u8; 4] {
    let x = u32::try_from(x).expect("on the output");
    let y = u32::try_from(y).expect("on the output");
    frame.pixel(x, y).expect("a pixel of the frame")
}

/// The keyboard binding a chord spells: `(["Super"], "left")`.
fn binding(modifiers: &[KeyboardModifier], key: &str) -> KeyboardBinding {
    KeyboardBinding::new(modifiers.to_vec(), KeyLegend::from_name(key))
}

/// Every chord the guide's essentials name.
fn essential_chords(guide: &Guide) -> Vec<KeyboardBinding> {
    guide
        .essentials
        .iter()
        .flatten()
        .flat_map(|(_, keys)| keys.clone())
        .collect()
}

#[test]
fn f1_shows_the_guide_filled_with_the_chords_the_session_answers() {
    let _settled = ui::motion::testing::settled();
    let mut session = one_display();
    assert!(!up(&session), "nothing is showing yet");

    tap(&mut session, KEY_F1);
    until(&mut session, up);
    assert!(
        session
            .shortcut_guide()
            .is_some_and(|seat| seat.presentation().is_drawn()),
        "something of the guide is on screen"
    );

    let guide = guide(&session);
    // The pairs are the shortcuts the person can change, in the guide's order,
    // with the chords the session answers them by now.
    let pairs: Vec<&str> = guide
        .pairs
        .iter()
        .map(|pair| pair.previous.action.as_str())
        .collect();
    assert_eq!(
        pairs,
        [
            "focus-previous-window",
            "previous-tab",
            "attach-tab-left",
            "maximize-window",
            "previous-strip",
            "previous-workspace",
            "move-group-left",
            "resize-narrower",
            "move-group-to-previous-strip",
        ]
    );
    assert_eq!(
        guide.pairs[0].previous.keys,
        binding(&[KeyboardModifier::System], "left"),
        "the live chord, not a default the guide remembers"
    );
    assert!(
        guide.pairs.iter().all(|pair| !pair.changed),
        "nothing is changed out of the box"
    );
    // The essentials name the keys that answer now, the fixed ones included.
    assert!(
        essential_chords(&guide).contains(&binding(&[], "printscreen")),
        "the screenshot key is an essential: {:?}",
        guide.essentials
    );
    assert!(
        essential_chords(&guide).contains(&binding(&[KeyboardModifier::System], "space")),
        "so is search: {:?}",
        guide.essentials
    );
    assert!(
        !guide.notes.is_empty(),
        "the guide says what else the keys do"
    );
    assert_eq!(guide.listening, None, "no shortcut waits for a chord");

    tap(&mut session, KEY_F1);
    until(&mut session, |session| !up(session));
}

/// The guide's Done, pressed as a person or a script presses it: the
/// dialog's primary control.
fn done(session: &mut Session) {
    session
        .invoke(
            "tilo.control:shortcut-guide.primary",
            "invoke",
            &serde_json::Map::new(),
        )
        .expect("the guide publishes its Done while it is up");
    session.step(Duration::from_millis(1)).expect("step");
}

#[test]
fn escape_and_done_take_the_guide_down() {
    let _settled = ui::motion::testing::settled();
    let mut session = one_display();

    tap(&mut session, KEY_F1);
    until(&mut session, up);
    tap(&mut session, KEY_ESCAPE);
    until(&mut session, |session| !up(session));

    tap(&mut session, KEY_F1);
    until(&mut session, up);
    done(&mut session);
    until(&mut session, |session| !up(session));
}

#[test]
fn the_guide_shows_the_chord_the_person_rebound() {
    let _settled = ui::motion::testing::settled();
    let mut config = SessionConfig::default();
    let mine = KeyChord::parse("Ctrl+Alt+J").expect("a chord");
    config
        .shortcuts
        .set(ShortcutAction::Search, mine)
        .expect("no other shortcut holds it");
    let mut session = session_with(config, &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)]);

    tap(&mut session, KEY_F1);
    until(&mut session, up);
    let guide = guide(&session);
    let chords = essential_chords(&guide);
    assert!(
        chords.contains(&binding(
            &[KeyboardModifier::Control, KeyboardModifier::Alt],
            "j"
        )),
        "search reads as the person bound it: {:?}",
        guide.essentials
    );
    assert!(
        !chords.contains(&binding(&[KeyboardModifier::System], "space")),
        "the default it left behind is gone: {:?}",
        guide.essentials
    );
}

#[test]
fn the_desktop_help_chord_shows_the_same_guide() {
    let _settled = ui::motion::testing::settled();
    let mut session = one_display();

    chord(&mut session, &[KEY_LEFT_META, KEY_LEFT_SHIFT, KEY_SLASH]);
    until(&mut session, up);
    assert!(
        session
            .shortcut_guide()
            .is_some_and(|seat| seat.presentation().is_open())
    );
    chord(&mut session, &[KEY_LEFT_META, KEY_LEFT_SHIFT, KEY_SLASH]);
    until(&mut session, |session| !up(session));
    assert!(!up(&session));
}

#[test]
fn the_guide_is_drawn_over_the_display_in_focus_and_leaves_it_clean() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    assert_eq!(displays(&session).len(), 2, "the desk tells both apart");
    focus(&mut session, 1);
    let centre = (WIDTH / 2, HEIGHT / 2);
    let before_left = shot(&mut session, "HEADLESS-1");
    let before_right = shot(&mut session, "HEADLESS-2");

    tap(&mut session, KEY_F1);
    until(&mut session, up);
    until(&mut session, |session| {
        session
            .shortcut_guide()
            .is_some_and(|seat| seat.presentation().is_drawn())
    });
    let after_left = shot(&mut session, "HEADLESS-1");
    let after_right = shot(&mut session, "HEADLESS-2");
    assert_ne!(
        pixel(&after_right, centre.0, centre.1),
        pixel(&before_right, centre.0, centre.1),
        "the guide is drawn on the display in focus"
    );
    assert_eq!(
        pixel(&after_left, centre.0, centre.1),
        pixel(&before_left, centre.0, centre.1),
        "and nothing of it reaches the other display"
    );

    tap(&mut session, KEY_F1);
    until(&mut session, |session| !up(session));
    until(&mut session, |session| {
        session
            .shortcut_guide()
            .is_some_and(|seat| !seat.presentation().is_drawn())
    });
    let closed = shot(&mut session, "HEADLESS-2");
    assert_eq!(
        pixel(&closed, centre.0, centre.1),
        pixel(&before_right, centre.0, centre.1),
        "the display is as it was once the guide is gone"
    );
}
