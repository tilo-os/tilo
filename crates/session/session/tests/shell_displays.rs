//! The tilolang shell on every display: one part per output, standing on its own display's
//! rectangle (each part follows an output added, moved or taken away), not stretched across the
//! displays' bounding box as one; and a part steps aside on the display a fullscreen window
//! owns, from the signal the compositor names those displays by.

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

mod common;

use std::time::Duration;

use common::window::Client;
use tilo_compositor::{OutputChange, OutputConfig};
use tilo_session::{Options, Session};

const WIDTH: i32 = 1000;
const HEIGHT: i32 = 700;
const PERSON: &str = "Pat";

fn options() -> Options {
    Options {
        presentation: ui::headless::fixture_runner_inputs,
        person: PERSON.to_owned(),
        collab: false,
        shell: true,
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
            output.position = Some(*place);
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
    common::wait_for(session, "what the test asked of the session", |session| {
        done(session).then_some(())
    });
}

/// Where the part on `output` stands, `(x, y, w, h)`, and whether it stepped aside: `None`
/// while no part stands there.
fn part(session: &Session, output: &str) -> Option<((i32, i32, u32, u32), bool)> {
    session
        .shell_parts()
        .into_iter()
        .find(|part| part.output == output)
        .map(|part| (part.rect, part.aside))
}

#[test]
fn every_display_has_a_shell_on_its_own_rectangle() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(&[(0, 0), (WIDTH, 0)]);
    until(&mut session, |session| session.shell_parts().len() == 2);
    let parts = session.shell_parts();
    let names: Vec<&str> = parts.iter().map(|part| part.output.as_str()).collect();
    assert_eq!(names, ["HEADLESS-1", "HEADLESS-2"], "one part per display");
    for part in &parts {
        assert!(
            !part.aside,
            "no window covers {}: its part stands",
            part.output
        );
    }
    let first = part(&session, "HEADLESS-1")
        .expect("the first display's part")
        .0;
    let second = part(&session, "HEADLESS-2")
        .expect("the second display's part")
        .0;
    assert_eq!(
        (first.0, first.1, first.2, first.3),
        (0, 0, WIDTH as u32, HEIGHT as u32),
        "the first part stands on the first display, its whole screen"
    );
    assert_eq!(
        (second.0, second.1, second.2, second.3),
        (WIDTH, 0, WIDTH as u32, HEIGHT as u32),
        "the second part stands on the second display, not stretched across both"
    );
    assert!(
        second.0 >= WIDTH,
        "the second part is on its display, beyond the first: {second:?}"
    );
}

#[test]
fn a_display_added_later_gets_its_own_shell() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(&[(0, 0)]);
    until(&mut session, |session| session.shell_parts().len() == 1);
    assert!(part(&session, "HEADLESS-1").is_some());
    // A second display comes: its part is made, on its rectangle.
    let mut output = OutputConfig::new("HEADLESS-2", WIDTH, HEIGHT);
    output.position = Some((WIDTH, 0));
    session.compositor().add_output(&output).unwrap();
    until(&mut session, |session| session.shell_parts().len() == 2);
    let second = part(&session, "HEADLESS-2")
        .expect("the new display's part")
        .0;
    assert_eq!(
        (second.0, second.2, second.3),
        (WIDTH, WIDTH as u32, HEIGHT as u32),
        "the added display's part stands on the added display"
    );
}

#[test]
fn a_display_turned_off_takes_its_part_and_brings_it_back() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(&[(0, 0), (WIDTH, 0)]);
    until(&mut session, |session| session.shell_parts().len() == 2);
    let offered = session.resources().len();
    session
        .compositor()
        .configure_outputs(&[OutputChange::disable("HEADLESS-2")])
        .unwrap();
    until(&mut session, |session| session.shell_parts().len() == 1);
    assert!(
        part(&session, "HEADLESS-1").is_some(),
        "the display left on keeps its part"
    );
    assert!(
        session.resources().len() < offered,
        "the gone display's part offers nothing on the door"
    );
    session
        .compositor()
        .configure_outputs(&[OutputChange::enable("HEADLESS-2")])
        .unwrap();
    until(&mut session, |session| session.shell_parts().len() == 2);
    assert_eq!(
        session.resources().len(),
        offered,
        "the display back has its own part again, not another one beside it"
    );
}

#[test]
fn a_part_steps_aside_on_the_display_a_fullscreen_window_owns() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(&[(0, 0), (WIDTH, 0)]);
    until(&mut session, |session| session.shell_parts().len() == 2);
    let mut client = Client::connect(session.compositor());
    client.pump(&mut session, |_, seen| {
        seen.wm_base.is_some() && seen.shm.is_some()
    });
    let full = client.map_plain(&mut session, "full", (300, 200));
    assert!(
        session.shell_parts().iter().all(|part| !part.aside),
        "a window that is not fullscreen leaves every part standing: {:?}",
        session.shell_parts()
    );

    // The window asks to cover its display, as a video player's full screen does.
    let (w, h) = client.seen.toplevel(full).size.unwrap_or((300, 200));
    client.toplevel(full).set_fullscreen(None);
    client.redraw_when_resized(&mut session, full, (w, h));
    client.pump(&mut session, |session, _| {
        !session.compositor().fullscreen_outputs().is_empty()
    });
    let owned = session.compositor().fullscreen_outputs();
    assert_eq!(owned.len(), 1, "the window covers one display: {owned:?}");
    let owned = &owned[0];
    client.pump(&mut session, |session, _| {
        part(session, owned).is_some_and(|(_, aside)| aside)
    });
    for place in session.shell_parts() {
        assert_eq!(
            place.aside,
            &place.output == owned,
            "only the part on the display the window owns steps aside: {place:?}"
        );
    }

    // It leaves fullscreen: the part is back on that display.
    let covered = client.seen.toplevel(full).size.unwrap_or((WIDTH, HEIGHT));
    client.toplevel(full).unset_fullscreen();
    client.redraw_when_resized(&mut session, full, covered);
    client.pump(&mut session, |session, _| {
        session.shell_parts().iter().all(|part| !part.aside)
    });
}

/// The shell on a `HiDPI` display draws at that display's scale: its part
/// paints the pixels the panel shows, so the bar and the desk are as crisp
/// as the display allows rather than one scale's worth stretched over
/// twice the area.
#[test]
fn a_shell_on_a_scaled_display_draws_at_that_scale() {
    let _settled = ui::motion::testing::settled();
    let mut session = Session::headless(
        options(),
        &[
            OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT).with_scale(1.0),
            OutputConfig::new("HEADLESS-2", WIDTH, HEIGHT)
                .with_scale(2.0)
                .at(WIDTH, 0),
        ],
    )
    .expect("session");
    until(&mut session, |session| session.shell_parts().len() == 2);
    let parts = session.shell_parts();
    let scale = |name: &str| {
        parts
            .iter()
            .find(|part| part.output == name)
            .map(|part| part.scale)
    };
    assert_eq!(scale("HEADLESS-1"), Some(1.0), "the plain display's part");
    assert_eq!(
        scale("HEADLESS-2"),
        Some(2.0),
        "the part on the 2 display draws at 2: {:?}",
        session.shell_parts()
    );
}

/// The shell's bar sits at the top of the display at every scale: its band
/// starts at the display's very top and covers the same logical height at 2
/// as at 1, rather than being a band drawn at one scale and stretched over
/// twice the area (which puts its edge where the panel's pixels say not).
#[test]
fn the_shell_bar_band_sits_at_the_top_of_the_display_at_every_scale() {
    let _settled = ui::motion::testing::settled();
    let mut bands = Vec::new();
    for (scale, pixels) in [(1.0, 1), (2.0, 2)] {
        let mut session = Session::headless(
            Options {
                presentation: ui::headless::fixture_runner_inputs,
                person: "Pat".to_owned(),
                shell: true,
                ..Options::default()
            },
            &[OutputConfig::new("HEADLESS-1", WIDTH * pixels, HEIGHT * pixels).with_scale(scale)],
        )
        .expect("session");
        for _ in 0..400 {
            session.settle_services();
            session.step(Duration::from_millis(1)).unwrap();
        }
        session.compositor().render().unwrap();
        let shot = session
            .compositor()
            .screenshot("HEADLESS-1")
            .expect("a frame");
        let x = shot.width * 3 / 4;
        let ground = shot.pixel(x, 4).expect("a pixel of the bar");
        let (mut first, mut last) = (None, 0);
        for y in 0..shot.height.min(600) {
            if shot.pixel(x, y) == Some(ground) {
                first.get_or_insert(y);
                last = y;
            }
        }
        let first = first.unwrap_or(0);
        bands.push((f64::from(first) / scale, f64::from(last + 1) / scale));
    }
    let (first_1, end_1) = bands[0];
    let (first_2, end_2) = bands[1];
    assert!(
        (first_2 - first_1).abs() <= 0.5,
        "the shell's bar starts at the same logical row at 1 and 2: {first_1} then {first_2}"
    );
    assert!(
        (end_2 - end_1).abs() <= 1.0,
        "the shell's bar covers the same logical height at 1 and 2: {end_1} then {end_2}"
    );
    assert!(
        first_1 <= 0.0 && first_2 <= 0.0,
        "the shell's bar ground reaches the display's very top edge, with no rule \
         a pixel inside it: it begins at logical {first_1} at 1 and {first_2} at 2"
    );
}

/// A bar flush with the display's edges paints them in its own colour: its
/// top row and its first and last columns are the body's colour at 2, with
/// no edge highlight along the screen's sides.
///
/// The theme's glass body lights a rim along its outline. On a bar edge to
/// edge with the screen that rim fell on the display's own top row and end
/// columns, reading as a glass line between the bar and the bezel rather
/// than as the bar reaching the edge.
#[test]
fn a_bar_flush_with_the_display_paints_its_edges_its_own_colour() {
    let _settled = ui::motion::testing::settled();
    let width = WIDTH * 2;
    let height = HEIGHT * 2;
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: "Pat".to_owned(),
            shell: true,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", width, height).with_scale(2.0)],
    )
    .expect("session");
    until(&mut session, |session| !session.shell_parts().is_empty());
    // The translucent style is where the glass rim shows.
    session
        .run_shell_setting("set_bar_finish", vec![serde_json::json!("translucent")])
        .expect("the bar takes the theme's glass");
    for _ in 0..120 {
        session.settle_services();
        session.step(Duration::from_millis(1)).unwrap();
    }
    session.compositor().render().unwrap();
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a frame");
    // The glass body lights a rim one pixel thick along its outline. On a
    // bar edge to edge with the screen that rim lands on the display's own
    // top row and end columns: compare each edge pixel with the one just
    // inside it, so the material's own sheen — a smooth gradient across the
    // body — is not read as an edge, and a one-pixel line of light is.
    let step = |a: Option<[u8; 4]>, b: Option<[u8; 4]>| -> i32 {
        let (a, b) = (a.expect("a pixel"), b.expect("a pixel"));
        (0..3)
            .map(|channel| i32::from(a[channel]) - i32::from(b[channel]))
            .max()
            .unwrap_or(0)
    };
    let mut wrong = Vec::new();
    for x in 0..shot.width {
        let rise = step(shot.pixel(x, 0), shot.pixel(x, 2));
        if rise > 2 {
            wrong.push((x, 0, rise));
        }
    }
    for y in 0..MIN_BAR_SAMPLES {
        for x in [0, shot.width - 1] {
            let inside = if x == 0 { 2 } else { shot.width - 3 };
            let rise = step(shot.pixel(x, y), shot.pixel(inside, y));
            if rise > 2 {
                wrong.push((x, y, rise));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "the bar's top row and its first and last columns carry no edge \
         highlight: each is at most 2 levels above the pixel inside it, and \
         {} are brighter, the first at {wrong:?}",
        wrong.len()
    );
}

/// How far down the bar's first and last columns the flush edges are read.
const MIN_BAR_SAMPLES: u32 = 24;
