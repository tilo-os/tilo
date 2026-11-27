//! A window on the shell fills the room the shell's own bar and dock leave: it starts under the
//! bar and stops at the dock's resting band, on the foot and on either side, and a dock that
//! hides itself leaves the window the whole display until the person reaches for it.
//!
//! The pictures are written to `$WINDOW_ROOM_OUT` (default `~/.cache/window-room/shots`): the
//! window beside the dock on the foot and up the leading edge, in both themes.

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

use serde_json::{Value, json};
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_session::{Options, Session};

use crate::common::{HEIGHT, WIDTH, on_screen, until};

/// The person's appearance section, whose published setters choose the theme.
const APPEARANCE: &str = "settings/appearance";
/// One display's refresh.
const REFRESH: Duration = Duration::from_millis(16);
/// The theme's own gaps (`RhythmScale::BASE`): the dock stands `related` in from the screen's
/// edge, and the strip's row keeps one `tight` gap from the dock's band.
const RELATED: f64 = 8.0;
const TIGHT: f64 = 4.0;

/// A headless session on this device's own state, the shell over the seeded apps, with the
/// appearance section beside them: what a development session draws on its display.
fn session() -> Session {
    let root = crate::common::private_home();
    let mut services = crate::common::seeded_apps();
    services.push(Box::new(settings_sections::service::SettingsService::new()));
    let mut session = Session::headless(
        Options {
            services,
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
    // A session whose shell has no display runs no action of it: the bar is drawn first.
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
/// frame a person sees, whose body the tab's content fills.
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

/// The window in front, once the shell has laid it out and it stands still: the shell measures
/// the room its chrome leaves on one frame, and the window takes it on a later one, so a single
/// frame of the same rect is not the shell's answer yet.
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

/// Choose the light theme, as the appearance section holds it.
fn light(session: &mut Session) {
    for (field, value) in [
        ("light_theme", json!("liquid-glass-light")),
        ("mode", json!("light")),
    ] {
        let arguments = json!({ "value": value });
        session
            .invoke(
                APPEARANCE,
                &settings_store::workspace::verb(field),
                arguments.as_object().expect("an object"),
            )
            .unwrap_or_else(|error| panic!("set {field}: {error:?}"));
        step(session);
    }
    until(session, "the light theme", |session| {
        session
            .read(APPEARANCE)
            .is_some_and(|section| section.get("mode") == Some(&json!("light")))
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

/// The bar's band, as the window's top meets it.
#[track_caller]
fn bar_band(session: &Session) -> (f64, f64) {
    let bar = on_screen(session, "Top bar").expect("the bar stands across the top");
    (bar.1, bar.1 + bar.3)
}

/// A window on the foot's dock starts at the bar's band and stops at the dock's: no bare desktop
/// between them, and the dock keeps its own band under the window.
#[test]
fn a_window_fills_the_room_the_bar_and_dock_leave_on_the_foot() {
    let mut session = session();
    let window = front_window(&mut session);
    let (_, bar_bottom) = bar_band(&session);
    let dock = on_screen(&session, "Dock").expect("the dock rides the foot");
    assert!(
        window.y0 >= bar_bottom - 0.5 && window.y0 <= bar_bottom + RELATED + 0.5,
        "the window starts at the bar's band {bar_bottom}: {window:?}"
    );
    assert!(
        window.y1 <= dock.1 + 0.5,
        "the window stops at the dock's band {}: {window:?}",
        dock.1
    );
    assert!(
        dock.1 - window.y1 <= RELATED + TIGHT + 0.5,
        "no bare desktop between the window and the dock at {}: {window:?}",
        dock.1
    );
    capture(&mut session, "room-foot-dark");
    light(&mut session);
    let window = front_window(&mut session);
    let dock = on_screen(&session, "Dock").expect("the dock rides the foot");
    assert!(
        window.y1 <= dock.1 + 0.5 && dock.1 - window.y1 <= RELATED + TIGHT + 0.5,
        "the light theme leaves the same room: {window:?} against the dock at {}",
        dock.1
    );
    capture(&mut session, "room-foot-light");
}

/// A window beside a dock on the leading edge reaches the bar at the top and the display's foot
/// at the bottom, and stops at the dock's band's trailing edge.
#[test]
fn a_window_fills_the_room_a_dock_beside_it_leaves() {
    let mut session = session();
    let _ = front_window(&mut session);
    dock_on(&mut session, "left");
    let window = front_window(&mut session);
    let (_, bar_bottom) = bar_band(&session);
    let dock = on_screen(&session, "Dock").expect("the dock rides the leading edge");
    assert!(
        window.y0 >= bar_bottom - 0.5 && window.y0 <= bar_bottom + RELATED + 0.5,
        "the window starts at the bar's band {bar_bottom}: {window:?}"
    );
    assert!(
        window.y1 >= 800.0 - 2.0 * RELATED,
        "the window reaches the display's foot with no dock under it: {window:?}"
    );
    assert!(
        window.x0 >= dock.0 + dock.2 - 0.5,
        "the window starts at the dock's band {}: {window:?}",
        dock.0 + dock.2
    );
    assert!(
        dock.0 - window.x1 <= RELATED + TIGHT + 0.5,
        "no bare desktop between the window and the dock at {}: {window:?}",
        dock.0
    );
    capture(&mut session, "room-left-dark");
    light(&mut session);
    let window = front_window(&mut session);
    let dock = on_screen(&session, "Dock").expect("the dock rides the leading edge");
    assert!(
        dock.0 - window.x1 <= RELATED + TIGHT + 0.5,
        "the light theme leaves the same room beside the dock: {window:?} at {}",
        dock.0
    );
    capture(&mut session, "room-left-light");
}

/// A dock that hides itself takes no room: the window fills the display to its foot, and reaching
/// for the dock draws it over the window without moving it.
#[test]
fn a_hidden_dock_leaves_the_window_the_whole_display() {
    let mut session = session();
    let _ = front_window(&mut session);
    session
        .run_shell_action("set_dock_showing", vec![Value::from("edge_reveal")])
        .expect("the dock hides itself");
    let window = front_window(&mut session);
    assert!(
        window.y1 >= 800.0 - 2.0 * RELATED,
        "the window reaches the display's foot: {window:?}"
    );
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
    capture(&mut session, "room-autohide-dark");
    session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: pointer.0,
            y: pointer.1,
        })
        .expect("the pointer back");
    step(&mut session);
}
