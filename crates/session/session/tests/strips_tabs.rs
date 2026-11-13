//! The Strips desk style's window tabs (`docs/designs/app-language/shell.md` §2, §3): a strip of
//! several places shows them as its window's tabs, and a person tabs one window's place into
//! another window's bar, switches between them by click and by the keyboard, and drags one back
//! out into a window of its own.
//!
//! Every one of them is an arranging action of the session's (`shell:arrangement`'s `move`, and
//! the desk's `split-right`), so a press, a key and an agent change the desk the same way,
//! attributed and undoable. What stands on screen is read from what the shell lays out for
//! assistive technology, never from pixels.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

mod common;

use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_compositor::{InputEvent, KeyRepeat, OutputConfig};
use tilo_session::{Options, Session};

use common::window::press_key;
use common::{HEIGHT, PERSON, TITLES, WIDTH, notebook as notes, on_screen, until};

/// The right arrow, evdev.
const KEY_RIGHT: u32 = 106;
/// The left arrow, evdev.
const KEY_LEFT: u32 = 105;

/// A session with the shell as the whole screen and the notes a test places.
fn session() -> (Session, Vec<String>) {
    let (notebook, notes) = notes(&TITLES);
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![Box::new(notebook)],
            ..Options::default()
        },
    );
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(1),
        rate: 0,
    });
    (session, notes)
}

/// Keep the screen now as `name.png` where `TL_SHELL_SHOTS` names, for the shots the report cites.
fn keep(session: &mut Session, name: &str) {
    let Some(dir) = std::env::var_os("TL_SHELL_SHOTS") else {
        return;
    };
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a screenshot of the display");
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("the captures' directory");
    image::save_buffer(
        dir.join(format!("{name}.png")),
        &shot.rgba,
        shot.width,
        shot.height,
        image::ColorType::Rgba8,
    )
    .expect("the capture written");
}

/// Place `resource` on the workspace in front, in a strip of its own: the place's id and the
/// strip's.
///
/// The place names only its resource, not a workspace, a row, a strip or a place id: the session
/// mints those against the surface in front. A device's ids reach past what a JSON number holds (a
/// person's own key salts them), so they travel as the desk's own, never through the call
/// (`place_here` in `shell_screen` places the same way, and the arrangement's own `place` reads
/// them from the surface when the call names none).
fn place(session: &mut Session, resource: &str) -> (u64, u64) {
    let before: Vec<String> = common::places(session)
        .into_iter()
        .map(|place| place.key)
        .collect();
    let arguments: Map<String, Value> = json!({
        "resource": resource,
        "x": 80,
        "y": 60,
        "width": 480,
        "height": 360,
    })
    .as_object()
    .unwrap()
    .clone();
    session
        .invoke("shell:arrangement", "place", &arguments)
        .unwrap_or_else(|error| panic!("place {resource}: {error:?}"));
    let made = common::wait_for(session, "the place is in the record", |session| {
        common::places(session)
            .into_iter()
            .find(|place| !before.contains(&place.key))
    });
    let id = common::number(&made.key);
    (id, common::number(&made.strip))
}

/// The strip the place `place` stands in, by the record's ids.
fn strip_of(session: &Session, place: u64) -> Option<u64> {
    common::strip_of(session, place).map(|strip| common::number(&strip.key))
}

/// The places of the strip `strip`, in the record's order.
fn places_of(session: &Session, strip: u64) -> Vec<u64> {
    let key = strip.to_string();
    common::places(session)
        .into_iter()
        .filter(|place| place.strip == key)
        .map(|place| common::number(&place.key))
        .collect()
}

/// The place in front of the strip `strip`, by the record's ids.
fn front_of(session: &Session, strip: u64) -> Option<u64> {
    common::strip(session, strip).map(|strip| strip.active.parse().expect("a place id"))
}

/// The name the shell shows the place under `title` by, as the session resolved it: what the
/// window's bar and its tab read.
fn shown_as(session: &Session, title: &str) -> String {
    common::arrangement(session)
        .places
        .into_iter()
        .find(|place| place.title == title)
        .map_or_else(
            || panic!("the desk holds a place for {title}"),
            |place| place.name,
        )
}

/// Tab the place `place` into the strip `strip`, as letting its window's bar go over that
/// window's bar does: the arrangement's published `move` with the strip it becomes a tab of.
fn tab_into(session: &mut Session, place: u64, strip: u64) {
    let row = common::strip(session, strip).expect("the strip reads").row;
    let arguments: Map<String, Value> = json!({
        "row": format!("shell:row:{row}"),
        "strip": format!("shell:strip:{strip}"),
        "before": Value::Null,
    })
    .as_object()
    .unwrap()
    .clone();
    session
        .invoke(&format!("shell:place:{place}"), "move", &arguments)
        .unwrap_or_else(|error| panic!("tab {place} into {strip}: {error:?}"));
    until(session, "the place is a tab of the strip", |session| {
        strip_of(session, place) == Some(strip)
    });
}

/// Two notes, the second tabbed into the first's window: both places, and the strip.
fn two_tabs(session: &mut Session, notes: &[String]) -> (u64, u64, u64) {
    until(session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    let (first, _own) = place(session, &notes[0]);
    let (second, _own) = place(session, &notes[1]);
    let strip = strip_of(session, first).expect("the first place stands in a strip");
    tab_into(session, second, strip);
    until(session, "both tabs", |session| {
        on_screen(session, &shown_as(session, TITLES[0])).is_some()
            && on_screen(session, &shown_as(session, TITLES[1])).is_some()
    });
    (first, second, strip)
}

/// Two notes open, the second tabbed into the first's window: the strip holds both places as its
/// tabs, in the record's order, with the tab that came in the one the window shows.
#[test]
fn a_note_tabbed_into_a_window_stands_as_its_tab() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    let (first, second, strip) = two_tabs(&mut session, &notes);
    assert_eq!(
        places_of(&session, strip),
        [first, second],
        "the strip holds both places as its tabs"
    );
    assert_eq!(
        front_of(&session, strip),
        Some(second),
        "the tab that came in stands in front"
    );
    let labels = session.accessible_labels();
    assert!(
        labels
            .iter()
            .any(|label| label == &shown_as(&session, TITLES[0])),
        "the first tab shows its place's title: {labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|label| label == &shown_as(&session, TITLES[1])),
        "the second tab shows its place's title: {labels:?}"
    );
    keep(&mut session, "strips-two-tabs");
}

/// Switching tabs by click: choosing a tab shows its place, and choosing the other one shows
/// that, as a window's tabs do.
#[test]
fn choosing_a_tab_shows_its_place() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    let (first, second, strip) = two_tabs(&mut session, &notes);
    let first_name = shown_as(&session, TITLES[0]);
    let second_name = shown_as(&session, TITLES[1]);

    common::press(&mut session, &first_name);
    until(&mut session, "the first tab in front", |session| {
        front_of(session, strip) == Some(first)
    });
    common::press(&mut session, &second_name);
    until(&mut session, "the second tab in front", |session| {
        front_of(session, strip) == Some(second)
    });
}

/// Switching tabs by keyboard: a press on a tab gives its window's tab list the keyboard, and the
/// arrow keys then walk the tabs — the standard tab list keys, which report the pill they reach
/// as the one chosen.
#[test]
fn the_arrow_keys_walk_a_windows_tabs() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    let (first, second, strip) = two_tabs(&mut session, &notes);
    let second_name = shown_as(&session, TITLES[1]);
    // The second tab is in front; a press on its pill puts the keyboard in the tab list.
    common::press(&mut session, &second_name);
    session.step(Duration::from_millis(16)).expect("step");
    // Right walks the tabs on: the list wraps to the first, and the strip follows it.
    press_key(&mut session, KEY_RIGHT, true);
    press_key(&mut session, KEY_RIGHT, false);
    until(&mut session, "the next tab in front", |session| {
        front_of(session, strip) == Some(first)
    });
    // Left walks back to the tab it started on.
    press_key(&mut session, KEY_LEFT, true);
    press_key(&mut session, KEY_LEFT, false);
    until(&mut session, "the tab before in front", |session| {
        front_of(session, strip) == Some(second)
    });
}

/// Closing a tab: the tab's close takes its place out of the strip and the window it is stays with
/// the place that was behind it.
#[test]
fn closing_a_tab_takes_its_place_out() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    let (first, second, strip) = two_tabs(&mut session, &notes);
    // The tab in front's close chip closes it. The window's own Close closes the place in front,
    // which is the tab that came in.
    let chip = format!("Close {}", shown_as(&session, TITLES[1]));
    common::press(&mut session, &chip);
    until(&mut session, "the tab closed", |session| {
        places_of(session, strip) == [first]
    });
    assert_eq!(
        front_of(&session, strip),
        Some(first),
        "the place that stayed is the one in front"
    );
    let _ = second;
}

/// A press, a travel of `steps` past the drag threshold, and a release: what a drag with the
/// pointer is, as a person makes one. The travel is split into `steps` so the toolkit sees the
/// pointer move the way a hand moves it.
fn drag(session: &mut Session, from: (f64, f64), to: (f64, f64)) {
    let pointer = |session: &mut Session, x: f64, y: f64| {
        session
            .compositor()
            .input(InputEvent::PointerMotion { x, y })
            .expect("the pointer");
        session.step(Duration::from_millis(16)).expect("step");
    };
    let button = |session: &mut Session, pressed: bool| {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: common::BTN_LEFT,
                pressed,
            })
            .expect("the button");
        session.step(Duration::from_millis(16)).expect("step");
    };
    pointer(session, from.0, from.1);
    button(session, true);
    for step in 1..=8 {
        let t = f64::from(step) / 8.0;
        pointer(
            session,
            from.0 + (to.0 - from.0) * t,
            from.1 + (to.1 - from.1) * t,
        );
    }
    button(session, false);
}

/// Dragging a tab pill out of its window's bar gives it a window of its own: the tab strip's own
/// `detach` reports the pill let go outside the bar, and the shell runs the desk's `split-right`
/// for the place it carried, so the place leaves the window into one of its own, right after it.
#[test]
fn dragging_a_tab_out_gives_it_its_own_window() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    let (first, second, strip) = two_tabs(&mut session, &notes);
    let second_name = shown_as(&session, TITLES[1]);
    let at = |rect: (f64, f64, f64, f64)| (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0);
    let pill = at(common::wait_for(
        &mut session,
        "the tab's pill",
        |session| on_screen(session, &second_name),
    ));
    let window = at(common::wait_for(
        &mut session,
        "the window's body",
        |session| on_screen(session, &shown_as(session, TITLES[0])),
    ));
    // The pill is dragged down out of the bar into the window's body: let go outside the bar, the
    // strip reports it as the tab let go of, which the shell splits into a window of its own.
    drag(&mut session, pill, (window.0, window.1 + 200.0));
    until(&mut session, "the tab has a window of its own", |session| {
        strip_of(session, second).is_some_and(|own| own != strip)
    });
    assert_eq!(
        places_of(&session, strip),
        [first],
        "the window it left keeps the other place"
    );
    let own = strip_of(&session, second).expect("the place stands in a strip");
    assert_eq!(
        places_of(&session, own),
        [second],
        "the place stands alone in its new window"
    );
    keep(&mut session, "strips-tab-dragged-out");
    let _ = KEY_LEFT;
}

/// Drag the window bar of the place shown under `title` onto the window of the strip `onto`, as a
/// person does: press its bar, travel past the drag threshold to the other window's bar, and let
/// go. The two windows must stand apart on the desk, so each bar is a target of its own.
#[test]
fn dragging_a_windows_bar_onto_another_tabs_it_in() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    // Two notes, each a window of its own, side by side on the desk.
    let (first, _own) = place(&mut session, &notes[0]);
    let (second, _own) = place(&mut session, &notes[1]);
    let strip_first = strip_of(&session, first).expect("the first place stands in a strip");
    let strip_second = strip_of(&session, second).expect("the second place stands in a strip");
    assert_ne!(
        strip_first, strip_second,
        "each place starts in a window of its own"
    );
    let first_name = shown_as(&session, TITLES[0]);
    let second_name = shown_as(&session, TITLES[1]);
    // Both windows' bars stand on the desk: the first place's window and the second's.
    let from = common::wait_for(&mut session, "the first window's bar", |session| {
        on_screen(session, &first_name)
    });
    let to = common::wait_for(&mut session, "the second window's bar", |session| {
        on_screen(session, &second_name)
    });
    // The bar's ground is under its title: a point on the title's pill, not on a control. Press
    // there, travel to the other window's bar, and let go.
    let at = |rect: (f64, f64, f64, f64)| (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0);
    drag(&mut session, at(from), at(to));
    // The dragged window's place becomes a tab of the window it was let go over.
    until(
        &mut session,
        "the place is a tab of the other window",
        |session| strip_of(session, first) == Some(strip_second),
    );
    assert_eq!(
        places_of(&session, strip_second),
        [second, first],
        "the tab moved in lands after the window's own tab"
    );
    assert_eq!(
        front_of(&session, strip_second),
        Some(first),
        "the tab moved in is the window's shown one"
    );
    keep(&mut session, "strips-bar-dragged-in");
}
