//! Floating a window over the desk in a real session: the chord and the window's own menu lift it
//! out of its strip and keep the place it stands at in the desk's record, the same record's undo
//! floats it again, and a float carried back over its strip shows the gap it would land in and
//! snaps into it.
//! The pictures are written to `$FLOAT_WINDOWS_OUT` (default `~/.cache/float-snap/shots` ): the
//! desk with the conversation floating over its strip, carried over the windows it covers, and the
//! gap it snaps into, in both themes.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::float_cmp,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

mod common;

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_desktop::desk_verbs::CHROME_RESOURCE;
use tilo_session::arrangement::StripReading;
use tilo_session::mount::shell::WINDOW_MENU;
use tilo_session::{Options, Session};
use ui::shortcut_table::{KeyChord, ShortcutAction, ShortcutConfig};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
/// One display's refresh: how often the session asks its clients for their next frame.
const REFRESH: Duration = Duration::from_millis(16);
/// The conversation the captures float, and the two windows it floats over.
const CHAT: &str = "so.tilo.lang.chat";
const MAIL: &str = "so.tilo.lang.mail";
const NOTES: &str = "so.tilo.lang.notes";
/// The person's appearance section: the theme is chosen through its own published `field-edit` , as
/// Settings does.
const APPEARANCE: &str = "settings:settings.appearance";
/// The left button of a two-and-more button pointer.
const BTN_LEFT: u32 = 0x110;
/// What a key sends, evdev codes.
const KEY_F: u32 = 33;
const KEY_LEFT_SHIFT: u32 = 42;
const KEY_LEFT_META: u32 = 125;

/// A session as a development one starts: the tilolang shell as the whole screen, the seeded apps
/// published, and the appearance section beside them.
fn session() -> Session {
    let apps = common::seeded_apps();
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: common::PERSON.to_owned(),
            shell: false,
            collab: false,
            search: tilo_session::SearchOptions {
                // The apps installed on this device, as a real session reads them: the launcher
                // opens them, and the desk names their windows by their own names.
                lists: tilo_session::Lists::fixed(tilo_session::AppLists::read()),
                ..tilo_session::SearchOptions::default()
            },
            services: vec![
                Box::new(apps),
                Box::new(tilo_settings_service::SettingsService::new()),
            ],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

fn step(session: &mut Session) {
    session.step(REFRESH).expect("a turn of the session");
}

/// Run frames until `done` holds.
#[track_caller]
fn until(session: &mut Session, what: &str, mut done: impl FnMut(&Session) -> bool) {
    for _ in 0..120 {
        if done(session) {
            return;
        }
        step(session);
    }
    panic!("{what} never happened");
}

/// Open `app` on the desk, the way the launcher opens one, and answer its tab's id once the desk
/// has it.
fn open(session: &mut Session, app: &str) -> u64 {
    let arguments = json!({ "app": app });
    session
        .invoke(
            CHROME_RESOURCE,
            "open-app",
            arguments.as_object().expect("an object"),
        )
        .unwrap_or_else(|error| panic!("{app} opens on the desk: {error:?}"));
    until(session, &format!("{app}'s window"), |session| {
        tab_of(session, app).is_some()
    });
    step(session);
    tab_of(session, app).expect("the app's window is on the desk")
}

/// The tab of `app` 's window, while the desk holds one.
fn tab_of(session: &Session, app: &str) -> Option<u64> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|tab| {
                tilo_desktop_model::apps::canonical_app_id(&tab.tile)
                    .is_some_and(|shown| shown == app)
            })
            .map(|tab| tab.id)
    })
}

/// Where the window `tab` is drawn on the display.
fn rect(session: &Session, tab: u64) -> ui::kurbo::Rect {
    session
        .handle()
        .window_rect(tab)
        .unwrap_or_else(|| panic!("the window {tab} is drawn"))
}

/// The window `tab` , as the desk's record reads it: the place it keeps, and whether it floats
/// above the desk.
fn held(session: &Session, tab: u64) -> StripReading {
    let reading = session.arrangement().borrow().reading();
    let key = tab.to_string();
    let place = reading
        .places
        .into_iter()
        .find(|place| place.key == key)
        .unwrap_or_else(|| panic!("the desk keeps a place for the window {tab}"));
    let strip = place.strip;
    reading
        .strips
        .into_iter()
        .find(|held| held.key == strip)
        .unwrap_or_else(|| panic!("the desk keeps the strip of the window {tab}"))
}

/// The windows of the strip holding `tab` , in the order it lays them out.
fn order(session: &Session, tab: u64) -> Vec<u64> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .find(|strip| {
                strip
                    .window_groups
                    .iter()
                    .any(|group| group.tabs.iter().any(|shown| shown.id == tab))
            })
            .map(|strip| {
                strip
                    .window_groups
                    .iter()
                    .filter_map(|group| group.tabs.first().map(|shown| shown.id))
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// The desk the captures show: a conversation, a letter and a note in one strip, the conversation
/// in front.
fn desk() -> Session {
    let mut session = session();
    step(&mut session);
    for app in [CHAT, MAIL, NOTES] {
        open(&mut session, app);
    }
    // The window in front is the conversation: the chord and the menu act on it.
    let chat = tab_of(&session, CHAT).expect("the conversation is open");
    let handle = session.handle().clone();
    let mut tree = handle.read(Clone::clone);
    let found = tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .find_map(|strip| {
            strip
                .window_groups
                .iter()
                .find(|group| group.tabs.iter().any(|shown| shown.id == chat))
                .map(|group| (strip.id, group.id))
        });
    if let Some((strip, group)) = found {
        tree.active_group.insert(strip, group);
    }
    // Three windows of a strip's own width all fit beside each other: each takes a narrow share of
    // the strip, as the person would set them.
    for strip in tree
        .workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.strips)
    {
        for group in &mut strip.window_groups {
            group.width = tilo_desktop_model::WindowWidth::Narrow;
        }
    }
    handle.replace_tree(tree);
    for _ in 0..20 {
        step(&mut session);
    }
    session
}

fn key(session: &mut Session, keycode: u32, pressed: bool) {
    session
        .compositor()
        .input(InputEvent::Key { keycode, pressed })
        .expect("the key");
    step(session);
}

/// Press `action` 's chord as a person presses it: the modifiers held, the key tapped, the
/// modifiers let go.
fn press(session: &mut Session, action: ShortcutAction) {
    let chord = ShortcutConfig::default()
        .bindings
        .get(&action)
        .unwrap_or_else(|| panic!("{action:?} is bound"))
        .clone();
    press_chord(session, &chord);
}

fn press_chord(session: &mut Session, chord: &KeyChord) {
    let held: Vec<u32> = chord
        .modifiers
        .iter()
        .map(|name| match name.as_str() {
            "Super" => KEY_LEFT_META,
            "Shift" => KEY_LEFT_SHIFT,
            other => panic!("no keycode for {other}"),
        })
        .collect();
    for keycode in &held {
        key(session, *keycode, true);
    }
    let pressed = match chord.key.as_str() {
        "f" => KEY_F,
        other => panic!("no keycode for {other}"),
    };
    key(session, pressed, true);
    key(session, pressed, false);
    for keycode in held.iter().rev() {
        key(session, *keycode, false);
    }
}

fn pointer_at(session: &mut Session, (x, y): (f64, f64)) {
    session
        .compositor()
        .input(InputEvent::PointerMotion { x, y })
        .expect("the pointer");
    step(session);
}

fn button(session: &mut Session, pressed: bool) {
    session
        .compositor()
        .input(InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed,
        })
        .expect("the button");
    step(session);
}

fn window_menu(session: &Session) -> Value {
    session
        .read(WINDOW_MENU)
        .expect("the session publishes the window menu")
}

/// The rows the window menu offers now, by id and label, in order.
fn rows(session: &Session) -> Vec<(String, String)> {
    window_menu(session)["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .filter(|row| row["separator"] != json!(true))
        .map(|row| {
            (
                row["id"].as_str().expect("an id").to_owned(),
                row["label"].as_str().expect("a label").to_owned(),
            )
        })
        .collect()
}

/// Open the window menu for `tab` , as the shell's menu does.
fn open_menu(session: &mut Session, tab: u64) {
    let arguments = json!({ "tab": tab.to_string() });
    session
        .invoke(
            WINDOW_MENU,
            "open",
            arguments.as_object().expect("an object"),
        )
        .expect("the window menu opens");
    // The menu is a surface of its own: give it the frames it takes to show and draw through the
    // compositor before what it shows is read or seen.
    for _ in 0..8 {
        step(session);
    }
}

/// Choose `row` of the open window menu, as the shell's menu does.
fn choose(session: &mut Session, row: &str) {
    let arguments = json!({ "row": row });
    session
        .invoke(
            WINDOW_MENU,
            "choose",
            arguments.as_object().expect("an object"),
        )
        .unwrap_or_else(|error| panic!("{row} is chosen: {error:?}"));
    step(session);
}

/// Set the field `field` of the appearance section to `value` through the section's own published
/// `field-edit` , as Settings does.
fn set_field(session: &mut Session, field: &str, value: &Value) {
    let arguments = json!({
        "edit": {"path": [{"field": field}], "op": "set", "value": value}
    });
    session
        .invoke(
            APPEARANCE,
            "field-edit",
            arguments.as_object().expect("an object"),
        )
        .unwrap_or_else(|error| panic!("set {field}: {error:?}"));
    step(session);
}

/// Choose the light theme, as the appearance section holds it.
fn light(session: &mut Session) {
    set_field(session, "light_theme", &json!("liquid-glass-light"));
    set_field(session, "mode", &json!("light"));
    until(session, "the light theme", |session| {
        session
            .read(APPEARANCE)
            .is_some_and(|section| section.get("mode") == Some(&json!("light")))
    });
}

/// Where the pictures are written.
fn shots() -> PathBuf {
    std::env::var_os("FLOAT_WINDOWS_OUT").map_or_else(
        || {
            let home = std::env::var("HOME").expect("HOME");
            PathBuf::from(home).join(".cache/float-snap/shots")
        },
        PathBuf::from,
    )
}

/// Write the screen as `<shots>/<name>.png` , with what it says beside it.
#[track_caller]
fn capture(session: &mut Session, name: &str) {
    let dir = shots();
    std::fs::create_dir_all(&dir).expect("the shots directory");
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("screenshot");
    image::save_buffer(
        dir.join(format!("{name}.png")),
        &shot.rgba,
        shot.width,
        shot.height,
        image::ColorType::Rgba8,
    )
    .expect("write the frame");
    std::fs::write(
        dir.join(format!("{name}.labels")),
        session.accessible_labels().join("\n"),
    )
    .expect("write the labels");
}

/// The chord lifts the window in front above the desk at the floating window's size, the place it
/// stands at is the desk's record, the window's own menu offers it back into its strip, and the
/// record's undo floats it again.
#[test]
fn the_chord_floats_the_window_and_the_record_keeps_where_it_stands() {
    let _settled = ui::motion::testing::settled();
    let mut session = desk();
    let chat = tab_of(&session, CHAT).expect("the conversation is open");
    let in_strip = rect(&session, chat);
    let before = held(&session, chat);
    assert!(!before.floating, "it stands in its strip to begin with");

    press(&mut session, ShortcutAction::ToggleFloat);
    until(&mut session, "the window floats", |session| {
        held(session, chat).floating
    });

    // The place it stands at is the record's: the floating size, its corner and how far it is from
    // it.
    let floating = held(&session, chat);
    assert_eq!(
        (floating.float_width, floating.float_height),
        (640, 480),
        "the floating window's own size"
    );
    assert_eq!(
        floating.row, before.row,
        "the row it was lifted out of is its own"
    );
    assert!(
        ["top-left", "top-right", "bottom-left", "bottom-right"]
            .contains(&floating.float_corner.as_str()),
        "it is placed from a corner: {:?}",
        floating.float_corner
    );

    // And the desk draws it there, at the floating size, on the display.
    let over = rect(&session, chat);
    assert_eq!(
        (over.width(), over.height()),
        (640.0, 480.0),
        "drawn at the floating size, not its strip's share"
    );
    assert_ne!(over, in_strip, "lifted off where the strip laid it out");
    assert!(
        over.x0 >= 0.0
            && over.y0 >= 0.0
            && over.x1 <= f64::from(WIDTH)
            && over.y1 <= f64::from(HEIGHT),
        "kept on the display: {over:?}"
    );

    capture(&mut session, "float-dark");

    // The window's own menu offers it back into its strip.
    open_menu(&mut session, chat);
    let offered = rows(&session);
    assert!(
        offered.contains(&(
            "window-snap-back".to_owned(),
            "Snap back into the strip".to_owned()
        )),
        "the menu of a floating window says how to put it back: {offered:?}"
    );
    assert!(
        !offered.iter().any(|(id, _)| id == "window-float"),
        "and does not offer to float what already floats: {offered:?}"
    );

    choose(&mut session, "window-snap-back");
    until(&mut session, "the window snaps back", |session| {
        !held(session, chat).floating
    });
    assert_eq!(
        rect(&session, chat),
        in_strip,
        "snapped back with no target returns it exactly where it was"
    );

    // The record's undo takes that one step back, and the window floats again.
    session
        .undo_arrangement()
        .expect("the desk undoes the step");
    step(&mut session);
    assert!(
        held(&session, chat).floating,
        "one undo floats the window again"
    );

    // The same, in the light theme, for the pictures.
    light(&mut session);
    capture(&mut session, "float-light");
}

/// A float carried back over its strip shows the gap it would land in while the hand is still down,
/// and letting go there snaps it into that gap as one step of the desk's record.
#[test]
fn a_float_carried_over_its_strip_snaps_into_the_gap_it_was_dropped_in() {
    let _settled = ui::motion::testing::settled();
    let mut session = desk();
    let chat = tab_of(&session, CHAT).expect("the conversation is open");
    let mail = tab_of(&session, MAIL).expect("a letter is open");
    let notes = tab_of(&session, NOTES).expect("a note is open");
    let lifted = held(&session, chat);

    press(&mut session, ShortcutAction::ToggleFloat);
    until(&mut session, "the window floats", |session| {
        held(session, chat).floating
    });
    let over = rect(&session, chat);
    let letter = rect(&session, mail);
    let note = rect(&session, notes);
    assert_eq!(
        order(&session, chat),
        vec![chat, mail, notes],
        "the conversation is the first window of the desk"
    );

    // The hand takes the float by the ground of its bar, right of its tabs,
    // and carries it over the strip.
    let grab = (over.x1 - 220.0, over.y0 + 22.0);
    pointer_at(&mut session, grab);
    button(&mut session, true);
    // Over the letter it covers, the float stands on its own ground and its own elevation: its bar
    // reads as its own, never as the letter's.
    pointer_at(&mut session, (letter.center().x, letter.center().y));
    for _ in 0..4 {
        step(&mut session);
    }
    capture(&mut session, "carried-dark");
    // The hand goes where the window would land: in the gap between the letter and the note, still
    // on the strip, so the strip shows it.
    let gap = f64::midpoint(letter.x1, note.x0);
    let carried = (gap, letter.y0 + 22.0);

    pointer_at(&mut session, carried);
    for _ in 0..4 {
        step(&mut session);
    }
    let carried_to = rect(&session, chat);
    assert_ne!(carried_to, over, "the float follows the hand");
    assert_eq!(
        (carried_to.x0, carried_to.y0),
        (
            over.x0 + (carried.0 - grab.0),
            over.y0 + (carried.1 - grab.1)
        ),
        "it moves with the hand, keeping the hold it was taken by: {carried_to:?}"
    );
    assert_eq!(
        (carried_to.width(), carried_to.height()),
        (640.0, 480.0),
        "still the floating size"
    );
    capture(&mut session, "snapping-dark");

    button(&mut session, false);
    until(&mut session, "the window snaps back", |session| {
        !held(session, chat).floating
    });
    assert_eq!(
        order(&session, chat),
        vec![mail, chat, notes],
        "dropped in the gap between the letter and the note"
    );
    assert_eq!(
        held(&session, chat).row,
        lifted.row,
        "still the row it was lifted out of"
    );
    assert!(
        session
            .arrangement()
            .borrow()
            .reading()
            .strips
            .iter()
            .all(|reading| !reading.floating),
        "and nothing of the desk floats"
    );
    capture(&mut session, "snapped-dark");

    // The same, in the light theme.
    light(&mut session);
    press(&mut session, ShortcutAction::ToggleFloat);
    until(&mut session, "the window floats again", |session| {
        held(session, chat).floating
    });
    let over = rect(&session, chat);
    let letter = rect(&session, mail);
    let note = rect(&session, notes);
    pointer_at(&mut session, (over.x1 - 220.0, over.y0 + 22.0));
    button(&mut session, true);
    pointer_at(
        &mut session,
        (f64::midpoint(letter.x1, note.x0), letter.y0 + 22.0),
    );
    for _ in 0..4 {
        step(&mut session);
    }
    capture(&mut session, "carried-light");
    button(&mut session, false);
    until(&mut session, "the window snaps back", |session| {
        !held(session, chat).floating
    });
    capture(&mut session, "snapped-light");
}
