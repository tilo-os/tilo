//! The strips style's pointer gestures on a window's edge (`docs/design-language.md`, "Window edge
//! resize"; `docs/designs/app-language/shell.md` §2, §3): a window in a row is resized by carrying
//! its trailing edge or, where a window stands before it, its leading edge, and Shift trades width
//! with the window across the border; a row of windows settles on a window's edge when it is
//! scrolled; and the size a window is taking reads while its edge is carried, in either theme.
//!
//! Every check reads what the session keeps — the layout's widths, the strip a place stands in,
//! and where the shell draws a window for assistive technology — never a pixel.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_session::{Options, Session};
use tilo_session_core::surface::Scroll;

use crate::common::window::press_key;
use crate::common::{HEIGHT, PERSON, TITLES, WIDTH, notebook as notes, on_screen, until};

/// The left Shift key, evdev.
const KEY_LEFTSHIFT: u32 = 42;
/// A wide display, where a row of windows is wider than the least each keeps.
const WIDE: i32 = 1920;

/// A session with the shell as the whole screen and the notes a test places.
fn session() -> (Session, Vec<String>) {
    let (notebook, notes) = notes(&TITLES);
    let session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            person: PERSON.to_owned(),
            collab: false,
            services: vec![Box::new(notebook)],
            ..Options::default()
        },
    );
    (session, notes)
}

/// A session with the shell on a wide display, where a row of two windows is wider than the least
/// a window keeps and has room for an edge to trade.
fn wide_session() -> (Session, Vec<String>) {
    let (notebook, notes) = notes(&TITLES);
    let session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDE, HEIGHT)],
        Options {
            person: PERSON.to_owned(),
            collab: false,
            services: vec![Box::new(notebook)],
            ..Options::default()
        },
    );
    (session, notes)
}

/// The appearance section the person's theme is chosen through.
const APPEARANCE: &str = "settings/appearance";

/// A session with the person's settings service beside the notes, so the shell can show the theme
/// a test asks for.
fn themed_session() -> (Session, Vec<String>) {
    let (notebook, notes) = notes(&TITLES);
    let services: Vec<Box<dyn tilo_service::Service>> = vec![
        Box::new(notebook),
        Box::new(settings_sections::service::SettingsService::new()),
    ];
    let session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            person: PERSON.to_owned(),
            collab: false,
            services,
            ..Options::default()
        },
    );
    (session, notes)
}

/// Choose `theme` the way a person does: the theme of its mode, then that mode.
fn choose_theme(session: &mut Session, light: bool, theme: &str) {
    // The settings service publishes its sections on a pass of its own.
    crate::common::wait_for(session, "the appearance section", |session| {
        session.read(APPEARANCE)
    });
    let key = if light { "light_theme" } else { "dark_theme" };
    for (field, value) in [
        (key, json!(theme)),
        ("mode", json!(if light { "light" } else { "dark" })),
    ] {
        let arguments: Map<String, Value> = json!({ "value": value }).as_object().unwrap().clone();
        session
            .invoke(
                APPEARANCE,
                &settings_store::workspace::verb(field),
                &arguments,
            )
            .unwrap_or_else(|error| panic!("set {field}: {error:?}"));
    }
    until(session, "the appearance holds it", |session| {
        let held = session.read(APPEARANCE).unwrap_or(Value::Null);
        held.get(key) == Some(&json!(theme))
            && held.get("mode") == Some(&json!(if light { "light" } else { "dark" }))
    });
}

/// Place `resource` on the workspace in front, in a strip of its own: the place's id and the
/// strip's.
fn place(session: &mut Session, resource: &str) -> (u64, u64) {
    let before: Vec<String> = crate::common::places(session)
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
        .invoke("shell:layout", "place", &arguments)
        .unwrap_or_else(|error| panic!("place {resource}: {error:?}"));
    let made = crate::common::wait_for(session, "the place is in the record", |session| {
        crate::common::places(session)
            .into_iter()
            .find(|place| !before.contains(&place.key))
    });
    let id = crate::common::number(&made.key);
    (id, crate::common::number(&made.strip))
}

/// The strip the place `place` stands in, by the record's ids.
fn strip_of(session: &Session, place: u64) -> Option<u64> {
    crate::common::strip_of(session, place).map(|strip| crate::common::number(&strip.key))
}

/// The share of its row the strip `strip` keeps, in per-mille.
fn width_of(session: &Session, strip: u64) -> i64 {
    crate::common::strip(session, strip).map_or_else(
        || panic!("the strip {strip} is in the record"),
        |strip| strip.width,
    )
}

/// The name the shell shows the place under `title` by, as the session resolved it.
fn shown_as(session: &Session, title: &str) -> String {
    crate::common::layout(session)
        .places
        .into_iter()
        .find(|place| place.title == title)
        .map_or_else(
            || panic!("the shell holds a place for {title}"),
            |place| place.name,
        )
}

/// A press, a travel of `steps` past the drag threshold, and a release: what a drag with the
/// pointer is, as a person makes one.
fn drag(session: &mut Session, from: (f64, f64), to: (f64, f64)) {
    carry(session, from, to);
    let_go(session);
}

/// Press at `from` and travel to `to`, the button still down: what is dragged is still in hand.
fn carry(session: &mut Session, from: (f64, f64), to: (f64, f64)) {
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
                button: crate::common::BTN_LEFT,
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
}

/// Let go of the button where the drag stands.
fn let_go(session: &mut Session) {
    session
        .compositor()
        .input(InputEvent::PointerButton {
            button: crate::common::BTN_LEFT,
            pressed: false,
        })
        .expect("the button");
    session.step(Duration::from_millis(16)).expect("step");
}

/// The window's own bar, by the name it says: the widest node the shell lays out with that name,
/// since the window's bar and the pill showing its place both say it.
fn bar_of(session: &mut Session, name: &str) -> (f64, f64, f64, f64) {
    crate::common::wait_for(session, "the window's bar", |session| {
        session
            .shell_controls(name)
            .into_iter()
            .max_by(|a, b| a.2.total_cmp(&b.2))
            .filter(|rect| rect.2 > 0.0)
    })
}

/// Where a window's trailing edge is, on its bar: the window's name names its bar, which runs
/// across its top and belongs to the shell, and the edge lies on its trailing side, clear of
/// the controls' pill.
fn trailing_edge((x, y, w, h): (f64, f64, f64, f64)) -> (f64, f64) {
    (x + w - 2.0, y + h / 2.0)
}

/// Where a window's leading edge is, on its bar: the border with the window before it.
fn leading_edge((x, y, _w, h): (f64, f64, f64, f64)) -> (f64, f64) {
    (x + 2.0, y + h / 2.0)
}

/// Carry a window's trailing edge by `by` pixels: press the edge, travel, let go.
fn carry_edge(session: &mut Session, name: &str, by: f64) {
    let rect = bar_of(session, name);
    let edge = trailing_edge(rect);
    drag(session, edge, (edge.0 + by, edge.1));
}

/// Carrying a window's trailing edge resizes it: its share of the row changes, and the window
/// beside it keeps the share it had, so the border stands where the pointer let it go.
#[test]
fn carrying_a_windows_edge_resizes_it_and_the_neighbour_keeps_its_share() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    let (_first, own) = place(&mut session, &notes[0]);
    let (_second, beside) = place(&mut session, &notes[1]);
    assert_ne!(own, beside, "each place starts in a window of its own");
    let before = width_of(&session, own);
    let neighbour = width_of(&session, beside);
    let name = shown_as(&session, TITLES[0]);
    carry_edge(&mut session, &name, 140.0);
    until(&mut session, "the window is wider", |session| {
        width_of(session, own) > before
    });
    assert!(
        width_of(&session, own) - before > 20,
        "the edge carried 140 px takes a real share: {before} then {}",
        width_of(&session, own)
    );
    assert_eq!(
        width_of(&session, beside),
        neighbour,
        "the window beside it never changed: only the grabbed edge's window did"
    );
    crate::common::shot(&mut session, "strips-edge-resized");
}

/// Three windows on a 1280 display each keep the least its row shares out — the `44rem` a bar,
/// its tabs and an app need — so the row stands wider than the display it is drawn on.
#[test]
fn three_windows_keep_their_least_width_and_stand_wider_than_the_display() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    let mut names = Vec::new();
    for (index, note) in notes.iter().take(3).enumerate() {
        place(&mut session, note);
        names.push(shown_as(&session, TITLES[index]));
    }
    // The least the row keeps a window at, in the shell's pixels: `44rem`.
    let least = 44.0 * f64::from(tilo_ui_base::tokens::REFERENCE_BODY_PX);
    let drawn: Vec<(f64, f64, f64, f64)> = names
        .iter()
        .map(|name| bar_of(&mut session, name))
        .collect();
    for (name, rect) in names.iter().zip(&drawn) {
        assert!(
            rect.2 >= least - 1.0,
            "the window {name} keeps the least width its row shares out: {drawn:?}, least {least}"
        );
    }
    // The three are wider than the display: the row keeps their least and stands past its edge,
    // so it has that far to scroll.
    let past = drawn
        .iter()
        .map(|rect| rect.0 + rect.2)
        .fold(0.0_f64, f64::max);
    assert!(
        past > f64::from(WIDTH) + 100.0,
        "the row stands wider than the display it is drawn on: {drawn:?}"
    );
}

/// A row wider than the screen settles on a window's edge after it is scrolled: a wheel's step
/// takes the row to the next window, so no rest leaves two windows cut.
#[test]
fn the_row_settles_on_a_window_edge_after_it_is_scrolled() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    // Four windows at 44rem each are wider than the screen: the row scrolls along them.
    let mut names = Vec::new();
    for (index, note) in notes.iter().take(4).enumerate() {
        place(&mut session, note);
        names.push(shown_as(&session, TITLES[index]));
    }
    let row_start = bar_of(&mut session, &names[0]).0;
    // The pointer rests over the row and one notch of the wheel turns toward the windows after
    // the first. The row offers each window's leading edge to settle on, so the notch lands the
    // second window's edge at the row's own edge.
    let over = bar_of(&mut session, &names[0]);
    // The pointer rests on the row itself, in the gap between the two windows: the shell's own
    // ground, which the row's scroll takes the wheel on.
    session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: over.0 + over.2 + 6.0,
            y: over.1 + over.3 + 120.0,
        })
        .expect("the pointer");
    session.step(Duration::from_millis(16)).expect("step");
    session
        .compositor()
        .input(InputEvent::PointerAxis(Scroll::Notches(1.0, 0.0)))
        .expect("the wheel");
    for _ in 0..40 {
        session.step(Duration::from_millis(16)).expect("step");
    }
    let placed: Vec<(f64, f64, f64, f64)> = names
        .iter()
        .map(|name| bar_of(&mut session, name))
        .collect();
    assert!(
        placed.iter().any(|rect| (rect.0 - row_start).abs() <= 1.5),
        "a window's leading edge stands at the row's edge after the scroll: {placed:?} beside {row_start}"
    );
}

/// While a window's edge is carried, the size it is taking is shown: the chip says the window's
/// width and height, read out for assistive technology as the pointer drags.
#[test]
fn the_size_chip_shows_the_size_while_the_edge_is_carried() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    let (_, own) = place(&mut session, &notes[0]);
    place(&mut session, &notes[1]);
    let before = width_of(&session, own);
    let name = shown_as(&session, TITLES[0]);
    let rect = bar_of(&mut session, &name);
    let edge = trailing_edge(rect);
    session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: edge.0,
            y: edge.1,
        })
        .expect("the pointer");
    session.step(Duration::from_millis(16)).expect("step");
    session
        .compositor()
        .input(InputEvent::PointerButton {
            button: crate::common::BTN_LEFT,
            pressed: true,
        })
        .expect("the button");
    session.step(Duration::from_millis(16)).expect("step");
    // Carried 120 px wider, without letting go.
    for step in 1..=6 {
        session
            .compositor()
            .input(InputEvent::PointerMotion {
                x: edge.0 + 20.0 * f64::from(step),
                y: edge.1,
            })
            .expect("the pointer");
        session.step(Duration::from_millis(16)).expect("step");
    }
    let taking = (rect.2 + 120.0).round() as i64;
    let said: Vec<i64> = session
        .accessible_labels()
        .iter()
        .filter_map(|label| label.split_once(" by ")?.0.trim().parse::<i64>().ok())
        .collect();
    assert!(
        said.iter().any(|width| (*width - taking).abs() <= 2),
        "the chip says the width the edge is taking ({taking}): {said:?}"
    );
    crate::common::shot(&mut session, "strips-edge-carried");
    session
        .compositor()
        .input(InputEvent::PointerButton {
            button: crate::common::BTN_LEFT,
            pressed: false,
        })
        .expect("the button");
    until(&mut session, "the resize landed", |session| {
        width_of(session, own) > before
    });
}

/// The window's share of its row, read by the place `place` stands in.
fn share_of_place(session: &Session, place: u64) -> i64 {
    width_of(
        session,
        strip_of(session, place).expect("the place stands in a strip"),
    )
}

/// Carrying a window's leading edge resizes the window before it: the border between two windows
/// is the earlier one's trailing edge, so dragging it left narrows the earlier window and widens
/// the one grabbed, whose own share stays what it was.
#[test]
fn carrying_a_windows_leading_edge_resizes_the_window_before_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = wide_session();
    until(&mut session, "the strips", |session| {
        crate::common::on_screen_at(session, "Showing Strips", WIDE, HEIGHT).is_some()
    });
    let (first, before_strip) = place(&mut session, &notes[0]);
    let (second, own) = place(&mut session, &notes[1]);
    assert_ne!(
        before_strip, own,
        "each place starts in a window of its own"
    );
    let before = share_of_place(&session, first);
    let own_before = share_of_place(&session, second);
    let name = shown_as(&session, TITLES[1]);
    let rect = bar_of(&mut session, &name);
    let edge = leading_edge(rect);
    // Carried left, the edge takes room from the window before it.
    drag(&mut session, edge, (edge.0 - 140.0, edge.1));
    until(
        &mut session,
        "the window before it is narrower",
        |session| share_of_place(session, first) < before,
    );
    assert_eq!(
        share_of_place(&session, second),
        own_before,
        "the window whose edge was carried keeps its own share"
    );
}

/// Shift carried on an edge trades width with the window across the border: what one gains the
/// other loses, and both shares are set by the one gesture.
#[test]
fn shift_carrying_an_edge_trades_width_with_the_window_beside_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = wide_session();
    until(&mut session, "the strips", |session| {
        crate::common::on_screen_at(session, "Showing Strips", WIDE, HEIGHT).is_some()
    });
    let (_first, own) = place(&mut session, &notes[0]);
    let (second, beside) = place(&mut session, &notes[1]);
    assert_ne!(own, beside, "each place starts in a window of its own");
    let before = width_of(&session, own);
    let beside_before = width_of(&session, beside);
    let name = shown_as(&session, TITLES[0]);
    let rect = bar_of(&mut session, &name);
    let edge = trailing_edge(rect);
    press_key(&mut session, KEY_LEFTSHIFT, true);
    drag(&mut session, edge, (edge.0 + 120.0, edge.1));
    press_key(&mut session, KEY_LEFTSHIFT, false);
    until(&mut session, "the window is wider", |session| {
        width_of(session, own) > before
    });
    until(
        &mut session,
        "the window beside it is narrower",
        |session| width_of(session, beside) < beside_before,
    );
    let _ = second;
}

/// The edge light and the size chip read the same in both themes: the shell the person shows, dark
/// and light, says the size an edge is taking, and the captures stand beside it.
#[test]
fn the_edge_light_and_size_chip_read_in_both_themes() {
    let _settled = tilo_ui_base::motion::testing::settled();
    for (light, theme, name) in [(false, "tilo-dark", "dark"), (true, "paper-light", "light")] {
        let (mut session, notes) = themed_session();
        choose_theme(&mut session, light, theme);
        until(&mut session, "the strips", |session| {
            on_screen(session, "Showing Strips").is_some()
        });
        place(&mut session, &notes[0]);
        place(&mut session, &notes[1]);
        let shown = shown_as(&session, TITLES[0]);
        let rect = bar_of(&mut session, &shown);
        let edge = trailing_edge(rect);
        session
            .compositor()
            .input(InputEvent::PointerMotion {
                x: edge.0,
                y: edge.1,
            })
            .expect("the pointer");
        session.step(Duration::from_millis(16)).expect("step");
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: crate::common::BTN_LEFT,
                pressed: true,
            })
            .expect("the button");
        session.step(Duration::from_millis(16)).expect("step");
        for step in 1..=6 {
            session
                .compositor()
                .input(InputEvent::PointerMotion {
                    x: edge.0 + 20.0 * f64::from(step),
                    y: edge.1,
                })
                .expect("the pointer");
            session.step(Duration::from_millis(16)).expect("step");
        }
        assert!(
            session
                .accessible_labels()
                .iter()
                .any(|label| label.contains(" by ")),
            "the size the edge is taking reads in the {name} theme"
        );
        crate::common::shot(&mut session, &format!("strips-edge-{name}"));
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: crate::common::BTN_LEFT,
                pressed: false,
            })
            .expect("the button");
        session.step(Duration::from_millis(16)).expect("step");
    }
}
