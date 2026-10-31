//! The shortcut guide: F1 and the desktop's help chord put it up over the
//! display the person is working on, filled with the chords the session
//! answers now, and the same key, Escape or its Done take it down again.
//! A navigation shortcut is changed from the guide: picked with Tab and
//! Enter, the next chord becomes its keys, and the session answers it at
//! once and in every session after.

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

use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig};
use tilo_actions::ResourceSpec;
use tilo_compositor::{InputEvent, OutputConfig, Screenshot};
use tilo_desktop::desktop_actions::{CHROME_RESOURCE, DISPLAY_KIND, SEARCH_RESOURCE};
use tilo_session::{Options, Session, SessionConfig};
use tilo_sheets::Seat as _;
use tilo_sheets::shortcut_guide::{Guide, Pair};
use ui::input::{KeyLegend, KeyboardBinding, KeyboardModifier};

const WIDTH: i32 = 1000;
const HEIGHT: i32 = 700;

/// What a key sends, evdev codes.
const KEY_ESCAPE: u32 = 1;
const KEY_TAB: u32 = 15;
const KEY_ENTER: u32 = 28;
const KEY_LEFT_CTRL: u32 = 29;
const KEY_K: u32 = 37;
const KEY_LEFT_SHIFT: u32 = 42;
const KEY_SLASH: u32 = 53;
const KEY_LEFT_ALT: u32 = 56;
const KEY_SPACE: u32 = 57;
const KEY_F1: u32 = 59;
const KEY_UP: u32 = 103;
const KEY_PAGEUP: u32 = 104;
const KEY_LEFT_META: u32 = 125;

/// The shortcut table is the person's settings, and the tests that change it
/// save there: they take turns rather than overwrite each other's table.
static SHORTCUTS: Mutex<()> = Mutex::new(());

/// The lock, with the settings holding the defaults a fresh device has.
fn take_turns() -> MutexGuard<'static, ()> {
    let guard = SHORTCUTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ShortcutConfig::default()
        .save()
        .expect("a fresh shortcut table");
    guard
}

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
        &[
            OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT).at(0, 0),
            OutputConfig::new("HEADLESS-2", WIDTH, HEIGHT).at(WIDTH, 0),
        ],
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

/// The guide's row whose first shortcut is `action`.
#[track_caller]
fn pair(guide: &Guide, action: &str) -> Pair {
    guide
        .pairs
        .iter()
        .find(|pair| pair.previous.action == action)
        .unwrap_or_else(|| panic!("the guide has no {action} row"))
        .clone()
}

/// The workspace the person is in, as the desk names it.
fn workspace(session: &Session) -> Option<u64> {
    session
        .handle()
        .read(|tree| tree.active_workspace().map(|workspace| workspace.id.0))
}

/// A second workspace, opened the way the bar opens one: the person is then
/// in it, with the one before it to go back to.
fn open_a_workspace(session: &mut Session) -> Option<u64> {
    let before = workspace(session);
    session
        .invoke(CHROME_RESOURCE, "new-workspace", &serde_json::Map::new())
        .expect("the desk opens a workspace");
    until(session, |session| workspace(session) != before);
    assert_ne!(workspace(session), before, "a new workspace opened");
    before
}

/// Whether the desk publishes the resource `id` now: the open search, say.
fn published(session: &Session, id: &str) -> bool {
    fn find(resources: &[ResourceSpec], id: &str) -> bool {
        resources
            .iter()
            .any(|resource| resource.id.as_str() == id || find(&resource.children, id))
    }
    find(&session.resources(), id)
}

/// Ask the guide for the new keys of the shortcut `action`, as a person does:
/// the first keycap holds the keyboard when the guide opens and every row
/// stands two, so Tab walks to the keycap and Enter takes it.
#[track_caller]
fn listen_for(session: &mut Session, action: &str) {
    let guide_now = guide(session);
    let row = guide_now
        .pairs
        .iter()
        .position(|pair| pair.previous.action == action || pair.next.action == action)
        .unwrap_or_else(|| panic!("the guide has no {action} row"));
    let keycap = row * 2 + usize::from(guide_now.pairs[row].next.action == action);
    for _ in 0..keycap {
        tap(session, KEY_TAB);
    }
    tap(session, KEY_ENTER);
    until(session, |session| {
        guide(session).listening.as_deref() == Some(action)
    });
}

/// Press the Reset a changed row offers, as a person does: Tab past that
/// row's own keycaps (and the Resets of the rows above) and take it.
#[track_caller]
fn reset_pair(session: &mut Session, action: &str) {
    let guide_now = guide(session);
    let row = guide_now
        .pairs
        .iter()
        .position(|pair| pair.previous.action == action)
        .unwrap_or_else(|| panic!("the guide has no {action} row"));
    let above: usize = guide_now.pairs[..row]
        .iter()
        .map(|pair| 2 + usize::from(pair.changed))
        .sum();
    for _ in 0..above + 2 {
        tap(session, KEY_TAB);
    }
    tap(session, KEY_ENTER);
    until(session, |session| !pair(&guide(session), action).changed);
}

/// Change the shortcut `action` to `chord`, pressed as a person presses it.
fn rebind(session: &mut Session, action: &str, keys: &[u32]) {
    tap(session, KEY_F1);
    until(session, up);
    listen_for(session, action);
    chord(session, keys);
    until(session, |session| guide(session).listening.is_none());
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

/// The chords the person presses for a shortcut of their own.
const MINE: &[u32] = &[KEY_LEFT_CTRL, KEY_LEFT_ALT, KEY_K];

#[test]
fn a_navigation_shortcut_takes_the_chord_pressed_for_it_in_the_guide() {
    let _settled = ui::motion::testing::settled();
    let _turn = take_turns();
    let mut session = one_display();
    let first = open_a_workspace(&mut session);
    assert_ne!(workspace(&session), first, "the person is in the new one");

    rebind(&mut session, "previous-workspace", MINE);
    let guide = guide(&session);
    assert_eq!(
        pair(&guide, "previous-workspace").previous.keys,
        binding(&[KeyboardModifier::Control, KeyboardModifier::Alt], "k"),
        "the row reads the keys just pressed"
    );
    assert!(
        pair(&guide, "previous-workspace").changed,
        "and differs from its default"
    );
    assert_eq!(guide.message, None, "nothing was refused");
    assert!(
        up(&session),
        "changing a shortcut leaves the guide up to change another"
    );

    // The chord answers at once, once the guide is out of its way.
    tap(&mut session, KEY_F1);
    until(&mut session, |session| !up(session));
    chord(&mut session, MINE);
    until(&mut session, |session| workspace(session) == first);

    // And the setting keeps it for the sessions to come.
    let kept = ShortcutConfig::load();
    assert_eq!(
        kept.bindings.get(&ShortcutAction::PreviousWorkspace),
        KeyChord::parse("Ctrl+Alt+K").ok().as_ref(),
        "the settings hold the chord the guide took"
    );
    let mut next = session_with(
        SessionConfig::load(),
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    );
    let first = open_a_workspace(&mut next);
    chord(&mut next, MINE);
    until(&mut next, |session| workspace(session) == first);
}

#[test]
fn a_chord_another_shortcut_holds_is_refused_by_its_name() {
    let _settled = ui::motion::testing::settled();
    let mut session = one_display();

    tap(&mut session, KEY_F1);
    until(&mut session, up);
    listen_for(&mut session, "previous-workspace");

    // Super+Up maximizes by default, so it is no shortcut to change here.
    chord(&mut session, &[KEY_LEFT_META, KEY_UP]);
    until(&mut session, |session| guide(session).message.is_some());
    let guide_now = guide(&session);
    let named = tilo_sheets::word("shortcut-name-maximize-window");
    let message = guide_now.message.clone().expect("the refusal is said");
    assert!(
        message.contains(&named),
        "the shortcut in the way: {message}"
    );
    assert_eq!(
        guide_now.listening.as_deref(),
        Some("previous-workspace"),
        "the guide keeps waiting for the new keys"
    );
    assert_eq!(
        pair(&guide_now, "previous-workspace").previous.keys,
        binding(&[KeyboardModifier::System], "pageup"),
        "nothing changed"
    );

    tap(&mut session, KEY_ESCAPE);
    until(&mut session, |session| guide(session).listening.is_none());
    assert_eq!(
        guide(&session).message,
        None,
        "Escape takes the refusal back"
    );
    assert!(up(&session), "and leaves the guide up");
}

#[test]
fn a_printable_key_alone_is_refused_for_what_it_would_break() {
    let _settled = ui::motion::testing::settled();
    let mut session = one_display();

    tap(&mut session, KEY_F1);
    until(&mut session, up);
    listen_for(&mut session, "previous-workspace");

    tap(&mut session, KEY_K);
    until(&mut session, |session| guide(session).message.is_some());
    assert_eq!(
        guide(&session).message,
        Some(tilo_sheets::word("guide-rebind-needs-modifier")),
        "a bare key would run instead of typing"
    );
    assert_eq!(
        guide(&session).listening.as_deref(),
        Some("previous-workspace"),
        "the guide keeps waiting"
    );
    assert_eq!(
        pair(&guide(&session), "previous-workspace").previous.keys,
        binding(&[KeyboardModifier::System], "pageup"),
        "nothing changed"
    );
}

#[test]
fn a_changed_pair_goes_back_to_its_defaults_from_the_guide() {
    let _settled = ui::motion::testing::settled();
    let _turn = take_turns();
    let mut session = one_display();
    let first = open_a_workspace(&mut session);

    rebind(&mut session, "previous-workspace", MINE);
    assert!(pair(&guide(&session), "previous-workspace").changed);

    // The row offers Reset once it differs, and pressing it puts the pair
    // back the way it was.
    reset_pair(&mut session, "previous-workspace");
    assert!(up(&session), "the guide stays up over the reset");
    let guide_now = guide(&session);
    assert_eq!(
        pair(&guide_now, "previous-workspace").previous.keys,
        binding(&[KeyboardModifier::System], "pageup"),
        "the default chord is back"
    );
    assert_eq!(guide_now.listening, None);

    let kept = ShortcutConfig::load();
    assert_eq!(
        kept.bindings.get(&ShortcutAction::PreviousWorkspace),
        ShortcutConfig::default()
            .bindings
            .get(&ShortcutAction::PreviousWorkspace),
        "and the settings keep the default too"
    );

    tap(&mut session, KEY_F1);
    until(&mut session, |session| !up(session));
    chord(&mut session, &[KEY_LEFT_META, KEY_PAGEUP]);
    until(&mut session, |session| workspace(session) == first);
}

#[test]
fn the_guide_holds_the_desktops_chords_back_while_it_is_up() {
    let _settled = ui::motion::testing::settled();
    let mut session = one_display();
    let first = open_a_workspace(&mut session);
    let second = workspace(&session);
    assert_ne!(second, first);

    tap(&mut session, KEY_F1);
    until(&mut session, up);
    chord(&mut session, &[KEY_LEFT_META, KEY_PAGEUP]);
    chord(&mut session, &[KEY_LEFT_META, KEY_SPACE]);
    session.step(Duration::from_millis(1)).expect("step");
    assert_eq!(
        workspace(&session),
        second,
        "the chord that changes workspaces waits until the guide is gone"
    );
    assert!(
        !published(&session, SEARCH_RESOURCE),
        "and search does not open behind the guide"
    );
    assert!(up(&session), "the guide is still up");

    tap(&mut session, KEY_F1);
    until(&mut session, |session| !up(session));
    chord(&mut session, &[KEY_LEFT_META, KEY_PAGEUP]);
    until(&mut session, |session| workspace(session) == first);
    chord(&mut session, &[KEY_LEFT_META, KEY_SPACE]);
    until(&mut session, |session| published(session, SEARCH_RESOURCE));
}
