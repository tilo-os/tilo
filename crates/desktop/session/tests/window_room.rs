//! A window on the shell fills the room the shell's own bar and dock leave: it stands the same gap
//! under the bar as above the dock, takes the row beside a dock and up the foot, and a dock that
//! hides itself leaves the window the whole display until the person reaches for it.
//!
//! The pictures are written to `$WINDOW_ROOM_OUT` (default `~/.cache/window-room/shots`): the
//! window with the dock on the foot and up the leading edge, and a hidden dock before and after
//! it is reached for.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call"
)]

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_session::{Options, Session};

use crate::common::{HEIGHT, WIDTH, on_screen, until};

/// One display's refresh.
const REFRESH: Duration = Duration::from_millis(16);
/// The theme's own gaps (`RhythmScale::BASE`): the dock and the bar stand `related` in from the
/// edge they ride, and the band keeps the stack's `tight` from either.
const RELATED: f64 = 8.0;
const TIGHT: f64 = 4.0;
/// The room the band keeps from the bar and from the dock's body: the stack's gap and the body's
/// own inset.
const GAP: f64 = RELATED + TIGHT;
/// How far a window's frame may fall short of its row: the two edges' own rounding.
const SLACK: f64 = 2.0;

/// A headless session on this device's own state: the tilolang shell as the whole screen, drawn
/// from this build (it is the session's own package), over the layout the person's document
/// keeps there. The room its chrome leaves is the shell's, so these tests read it without a
/// development session's apps.
fn session() -> Session {
    let root = crate::common::private_home();
    let mut session = Session::headless(
        Options {
            services: vec![Box::new(settings_sections::service::SettingsService::new())],
            state: Some(root.path().join("state")),
            ..crate::common::api_options(root.path())
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("a session");
    // One window on the strip the shell opens on, as the person's document keeps it, and the
    // desktop's chrome: its bar lists the window in front's commands and its dock rides the foot.
    session
        .handle()
        .replace_tree(crate::common::tree::layout_tree());
    until(&mut session, "the shell to draw", |session| {
        !session.shell_parts().is_empty()
    });
    session
        .run_shell_action("apply", vec![Value::from("windows_desktop")])
        .expect("the shell shows the traditional desktop");
    until(&mut session, "the traditional desktop", |session| {
        on_screen(session, "Dock").is_some() && crate::common::preset_menu(session).is_none()
    });
    session
}

fn step(session: &mut Session) {
    session.step(REFRESH).expect("a turn of the session");
}

/// Where the shell draws the window in front's frame on the display, while it draws one: the
/// frame a person sees, whose body the app's window fills.
fn frame(session: &Session) -> Option<ui::kurbo::Rect> {
    session
        .controls()
        .into_iter()
        .find(|control| control.name.ends_with("/WindowFrame[0]"))
        .map(|control| {
            let (x, y, width, height) = control.rect;
            ui::kurbo::Rect::new(x, y, x + width, y + height)
        })
}

/// The window in front, once the shell has laid it out and it stands still: the shell measures the
/// room its chrome leaves on one frame, and the window takes it on a later one, so a single frame
/// of the same rect is not the shell's answer yet.
fn front_window(session: &mut Session) -> ui::kurbo::Rect {
    until(session, "the window's frame", |session| {
        frame(session).is_some()
    });
    let mut before = frame(session).expect("the window's frame");
    let mut still = 0;
    until(session, "the shell's room", |session| {
        let now = frame(session).unwrap_or(before);
        let same = (now.x0 - before.x0).abs() < 0.5
            && (now.y0 - before.y0).abs() < 0.5
            && (now.x1 - before.x1).abs() < 0.5
            && (now.y1 - before.y1).abs() < 0.5;
        still = if same { still + 1 } else { 0 };
        before = now;
        still >= 3
    });
    frame(session).expect("the window's frame")
}

/// The bar's band, as the window's top meets it.
#[track_caller]
fn bar_band(session: &Session) -> (f64, f64) {
    let bar = on_screen(session, "Top bar").expect("the bar stands across the top");
    (bar.1, bar.1 + bar.3)
}

/// Move the dock to `edge`, as the settings page's dock card does.
fn dock_on(session: &mut Session, edge: &str) {
    session
        .run_shell_action("dock_on", vec![Value::from(edge)])
        .unwrap_or_else(|why| panic!("the dock moves to the {edge}: {why}"));
    step(session);
    until(session, &format!("the dock on the {edge}"), |session| {
        let dock = on_screen(session, "Dock");
        dock.is_some_and(|(x, _, width, _)| {
            if edge == "left" {
                x < 120.0
            } else {
                x + width > 1160.0
            }
        })
    });
}

/// Where the pictures are written.
fn shots() -> PathBuf {
    std::env::var_os("WINDOW_ROOM_OUT").map_or_else(
        || {
            let home = std::env::var("HOME").expect("HOME");
            PathBuf::from(home).join(".cache/window-room/shots")
        },
        PathBuf::from,
    )
}

/// Write the screen as `<shots>/<name>.png`.
#[track_caller]
fn capture(session: &mut Session, name: &str) {
    let dir = shots();
    std::fs::create_dir_all(&dir).expect("the shots directory");
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a screenshot");
    image::save_buffer(
        dir.join(format!("{name}.png")),
        &shot.rgba,
        shot.width,
        shot.height,
        image::ColorType::Rgba8,
    )
    .expect("write the frame");
}

/// A window on the foot's dock fills the room: the same gap under the bar as above the dock, and
/// the row's whole width.
#[test]
fn a_window_fills_the_room_the_bar_and_dock_leave_on_the_foot() {
    let mut session = session();

    let window = front_window(&mut session);
    let (_, bar_bottom) = bar_band(&session);
    let dock = on_screen(&session, "Dock").expect("the dock rides the foot");
    assert!(
        (window.y0 - bar_bottom - GAP).abs() <= 1.0,
        "the window stands the theme's gap under the bar at {bar_bottom}: {window:?}"
    );
    assert!(
        (dock.1 - window.y1 - GAP).abs() <= 1.0,
        "the window stands the theme's gap above the dock at {}: {window:?}",
        dock.1
    );
    assert!(
        window.width() >= 1280.0 - 48.0,
        "a lone window fills its row, less the page's own insets: {window:?}"
    );
    capture(&mut session, "room-foot-dark");
}

/// A window beside a dock on the leading edge reaches the bar at the top and the display's foot
/// at the bottom, fills the row to the trailing edge, and stands no wallpaper band from the dock.
#[test]
fn a_window_fills_the_room_a_dock_beside_it_leaves() {
    let mut session = session();

    dock_on(&mut session, "left");
    let window = front_window(&mut session);
    let (_, bar_bottom) = bar_band(&session);
    let dock = on_screen(&session, "Dock").expect("the dock rides the leading edge");
    let dock_edge = dock.0 + dock.2;
    assert!(
        (window.y0 - bar_bottom - GAP).abs() <= 1.0,
        "the window stands the theme's gap under the bar at {bar_bottom}: {window:?}"
    );
    assert!(
        window.y1 >= 800.0 - GAP - SLACK,
        "the window reaches the display's foot with no dock under it: {window:?}"
    );
    assert!(
        window.x0 >= dock_edge - 0.5 && window.x0 - dock_edge <= 3.0 * RELATED,
        "the window follows the dock's band at {dock_edge} with no bare desktop: {window:?}"
    );
    assert!(
        window.x1 >= 1280.0 - GAP - SLACK,
        "the window reaches the display's trailing edge: {window:?}"
    );
    assert!(
        window.width() >= 1280.0 - dock.2 - 96.0,
        "the window fills the room the dock leaves: {window:?} beside a dock {} wide",
        dock.2
    );
    capture(&mut session, "room-left-dark");
}

/// A dock that hides itself takes no room: the window fills the display to its foot, and reaching
/// for the dock draws it over the window without moving it.
#[test]
fn a_hidden_dock_leaves_the_window_the_whole_display() {
    let mut session = session();

    session
        .run_shell_action("set_dock_showing", vec![Value::from("edge_reveal")])
        .expect("the dock hides itself");
    // The dock goes before the window grows: the shell measures the room its chrome leaves.
    until(&mut session, "the dock hidden", |session| {
        on_screen(session, "Dock").is_none()
    });
    let window = front_window(&mut session);
    assert!(
        window.y1 >= 800.0 - GAP - SLACK,
        "the window reaches the display's foot: {window:?}"
    );
    capture(&mut session, "room-autohide-hidden-dark");
    // Reach for the dock: it comes out over the window, which stays where it was.
    let pointer = session.compositor().pointer_location();
    session
        .compositor()
        .input(InputEvent::PointerMotion { x: 640.0, y: 798.0 })
        .expect("the pointer at the foot");
    until(&mut session, "the dock reached for", |session| {
        on_screen(session, "Dock").is_some()
    });
    assert_eq!(
        frame(&session),
        Some(window),
        "the reached-for dock draws over the window without moving it"
    );
    capture(&mut session, "room-autohide-reached-dark");
    session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: pointer.0,
            y: pointer.1,
        })
        .expect("the pointer back");
    step(&mut session);
    let _ = HEIGHT;
}
