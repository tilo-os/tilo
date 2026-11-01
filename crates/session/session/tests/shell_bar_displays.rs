//! The tilolang shell's bar on every display: one bar per output, standing on its own
//! display's rectangle (each part follows an output added, moved or taken away), not stretched
//! across the displays' bounding box as one.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::time::Duration;

use tilo_compositor::OutputConfig;
use tilo_session::{Options, Session};

const WIDTH: i32 = 1000;
const HEIGHT: i32 = 700;
const PERSON: &str = "Pat";

fn options() -> Options {
    Options {
        presentation: ui::headless::fixture_runner_inputs,
        person: PERSON.to_owned(),
        collab: false,
        ..Options::default()
    }
}

/// A session on `outputs`, each `WIDTH` x `HEIGHT` at its place.
fn session(places: &[(i32, i32)]) -> Session {
    let outputs: Vec<OutputConfig> = places
        .iter()
        .enumerate()
        .map(|(index, place)| {
            let mut output = OutputConfig::new(format!("HEADLESS-{}", index + 1), WIDTH, HEIGHT);
            output.position = *place;
            output
        })
        .collect();
    let mut session = Session::headless(options(), &outputs).expect("session");
    session.step(Duration::from_millis(1)).unwrap();
    session
}

/// Run frames until `done` holds.
#[track_caller]
fn until(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) {
    for _ in 0..2000 {
        session.step(Duration::from_millis(1)).expect("step");
        if done(session) {
            return;
        }
    }
    panic!(
        "condition not reached; the bars say: {:?}",
        session.shell_bars()
    );
}

/// Where the bar on `output` stands, `(x, y, w, h)`, and whether it stepped aside: `None`
/// while no bar stands there.
fn bar(session: &Session, output: &str) -> Option<((i32, i32, u32, u32), bool)> {
    session
        .shell_bars()
        .into_iter()
        .find(|bar| bar.output == output)
        .map(|bar| (bar.rect, bar.aside))
}

#[test]
fn every_display_has_a_bar_on_its_own_rectangle() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(&[(0, 0), (WIDTH, 0)]);
    until(&mut session, |session| session.shell_bars().len() == 2);
    let bars = session.shell_bars();
    let names: Vec<&str> = bars.iter().map(|bar| bar.output.as_str()).collect();
    assert_eq!(names, ["HEADLESS-1", "HEADLESS-2"], "one bar per display");
    for bar in &bars {
        assert!(
            !bar.aside,
            "no window covers {}: its bar stands",
            bar.output
        );
    }
    let first = bar(&session, "HEADLESS-1")
        .expect("the first display's bar")
        .0;
    let second = bar(&session, "HEADLESS-2")
        .expect("the second display's bar")
        .0;
    assert_eq!(
        (first.0, first.1, first.2),
        (0, 0, WIDTH as u32),
        "the first bar stands on the first display, its band high"
    );
    assert_eq!(
        (second.0, second.1, second.2),
        (WIDTH, 0, WIDTH as u32),
        "the second bar stands on the second display, not stretched across both"
    );
    assert_eq!(first.3, second.3, "both bands are the bar's own height");
    assert!(
        second.0 >= WIDTH,
        "the second bar is on its display, beyond the first: {second:?}"
    );
}

#[test]
fn a_display_added_later_gets_its_own_bar() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(&[(0, 0)]);
    until(&mut session, |session| session.shell_bars().len() == 1);
    assert!(bar(&session, "HEADLESS-1").is_some());
    // A second display comes: its bar is made, on its rectangle.
    let mut output = OutputConfig::new("HEADLESS-2", WIDTH, HEIGHT);
    output.position = (WIDTH, 0);
    session.compositor().add_output(&output).unwrap();
    until(&mut session, |session| session.shell_bars().len() == 2);
    let second = bar(&session, "HEADLESS-2")
        .expect("the new display's bar")
        .0;
    assert_eq!(
        (second.0, second.2),
        (WIDTH, WIDTH as u32),
        "the added display's bar stands on the added display"
    );
}

// The fullscreen step-aside is proven in `tilo-surface-host`'s `ShellPart` unit tests and
// lands with the desktop's own fullscreen grant (the strip policy calls none yet).
