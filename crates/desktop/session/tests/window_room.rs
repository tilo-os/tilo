//! A window on the shell stands the same gap under the shell's bar as above its dock's band: the
//! band between them is inset alike from both, so the page is as far from the bar as from the
//! dock.

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

use std::time::Duration;

use serde_json::Value;
use tilo_compositor::OutputConfig;
use tilo_session::{Options, Session};

use crate::common::{HEIGHT, WIDTH, on_screen, until};

/// One display's refresh.
const REFRESH: Duration = Duration::from_millis(16);
/// The room between the band and the chrome on either side of it: the theme's `related` inset and
/// the shell's `tight` gap (`RhythmScale::BASE`).
const GAP: f64 = 12.0;

/// A headless session on this device's own state, the shell over the seeded apps: what a
/// development session draws on its display, with the desktop's bar and dock.
fn session() -> Session {
    let root = crate::common::private_home();
    let mut session = Session::headless(
        Options {
            services: crate::common::seeded_apps(),
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

/// The bar's band, as the window's top meets it.
#[track_caller]
fn bar_band(session: &Session) -> (f64, f64) {
    let bar = on_screen(session, "Top bar").expect("the bar stands across the top");
    (bar.1, bar.1 + bar.3)
}

/// The window in front stands the same gap under the bar as above the dock's resting band.
#[test]
fn a_window_stands_the_same_gap_under_the_bar_as_above_the_dock() {
    let mut session = session();
    let window = front_window(&mut session);
    let (_, bar_bottom) = bar_band(&session);
    let dock = on_screen(&session, "Dock").expect("the dock rides the foot");
    let above = window.y0 - bar_bottom;
    let below = dock.1 - window.y1;
    assert!(
        (above - GAP).abs() <= 1.0,
        "the window stands the theme's gap under the bar at {bar_bottom}: {window:?}"
    );
    assert!(
        (below - GAP).abs() <= 1.0,
        "the window stands the theme's gap above the dock at {}: {window:?}",
        dock.1
    );
    step(&mut session);
}
