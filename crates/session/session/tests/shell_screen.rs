//! The tilolang shell as the whole screen of the native session (headless backend): the
//! shell's arrangement root stands over every display, its bar, its dock and the person's
//! arrangement as their preset shows it, every window drawn in its place's well.
//!
//! A note goes onto the canvas through the arrangement's published `place`, as an agent puts
//! it there. The preset changes through the shell's own `show_preset`, which the preset switch
//! over the arrangement runs. What stands on screen is read from what the shell lays out for
//! assistive technology, never from pixels.
//!
//! With the shell mounted as a bar only, none of the arrangement stands on screen: no preset
//! switch, and the note placed is nowhere.

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

use serde_json::{Map, Value, json};
use tilo_compositor::OutputConfig;
use tilo_session::{Options, Session};
use tilo_surface_host::shell_part::ShellLayer;
use tilolang_tilo::records_service::RecordService;

use common::{
    HEIGHT, NOTE_TITLE, PERSON, TITLES, WIDTH, notebook as notes, on_screen, press, reached, until,
};

/// The framed canvas's entry, named for what it holds: the text typed to find what to place.
const CANVAS_ENTRY: &str = "Typed";

/// A notebook kept by the notes app, published as the session publishes a workspace's records,
/// and the notes in it titled [`TITLES`], made through the app's own actions: their ids, in order.
fn notebook() -> (RecordService, Vec<String>) {
    notes(&TITLES)
}

fn session(layer: ShellLayer) -> (Session, String) {
    let (session, notes) = session_at(layer, WIDTH, HEIGHT);
    (session, notes.into_iter().next().unwrap())
}

/// The session with the shell as `layer`, on one display `width` by `height`, and the notes
/// [`notebook`] keeps.
fn session_at(layer: ShellLayer, width: i32, height: i32) -> (Session, Vec<String>) {
    let (notebook, notes) = notebook();
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: Some(layer),
            collab: false,
            services: vec![Box::new(notebook)],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", width, height)],
    )
    .expect("session");
    (session, notes)
}

/// Place `resource` on the canvas of the workspace in front, through the arrangement's
/// published `place`.
fn place(session: &mut Session, resource: &str) -> u64 {
    let (workspace, row, strip, place) = {
        let tree = session.handle().read(Clone::clone);
        let workspace = tree.active_workspace().expect("a workspace is in front");
        (
            workspace.id.0.to_string(),
            workspace.strips.first().expect("a row").id.0,
            session.handle().mint_id(),
            session.handle().mint_id(),
        )
    };
    let arguments: Map<String, Value> = json!({
        "workspace": workspace,
        "row": row,
        "strip": strip,
        "place": place,
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
    place
}

/// The whole screen: the bar's controls stand on it, the preset changes what stands there, and
/// a note placed on the canvas stands on it as the notes app draws it.
#[test]
fn the_shell_is_the_screen_and_its_arrangement_follows_the_published_actions() {
    let _settled = ui::motion::testing::settled();
    let (mut session, note) = session(ShellLayer::Screen);

    // The desk is up: its bar's controls stand on screen, over the whole output.
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let bars = session.shell_bars();
    assert_eq!(
        bars.len(),
        1,
        "one shell part, on the one display: {bars:?}"
    );
    let (_, _, width, height) = bars[0].rect;
    assert_eq!(
        (i64::from(width), i64::from(height)),
        (i64::from(WIDTH), i64::from(HEIGHT)),
        "the shell covers the display"
    );
    // Showing the desk as the framed canvas is the shell's own action, which the preset
    // switch over the arrangement runs. The canvas's entry (typed text finds something and
    // places it on the canvas) stands on it; the strips have none.
    until(&mut session, "the preset switch", |session| {
        on_screen(session, "Frames").is_some()
    });
    assert!(
        on_screen(&session, CANVAS_ENTRY).is_none(),
        "the strips stand first, not the canvas"
    );
    press(&mut session, "Frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS_ENTRY).is_some()
    });
    let entry = on_screen(&session, CANVAS_ENTRY).unwrap();

    // A note placed through the arrangement stands on the canvas, drawn by its app: its title
    // on screen, under the canvas's entry.
    place(&mut session, &note);
    until(&mut session, "the note's title on the canvas", |session| {
        on_screen(session, NOTE_TITLE).is_some()
    });
    let title = on_screen(&session, NOTE_TITLE).unwrap();
    assert!(
        title.1 >= entry.1 + entry.3,
        "the note stands on the canvas, under its entry: {title:?} under {entry:?}"
    );
}

/// With the shell as a bar only, the same action changes the record and nothing of the
/// arrangement stands on screen: no preset switch, no strips, no place. What the screen adds.
#[test]
fn as_a_bar_the_shell_shows_none_of_the_arrangement() {
    let _settled = ui::motion::testing::settled();
    let (mut session, note) = session(ShellLayer::Bar);
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let place = place(&mut session, &note).to_string();
    assert!(
        !reached(&mut session, |session| {
            on_screen(session, "Frames").is_some() || on_screen(session, &place).is_some()
        }),
        "a bar shows none of the arrangement"
    );
}

/// The overview, run as the touchpad's pinch, the overview's key and an agent run it (the
/// chrome's published `overview`, through the door), is the shell's own on the shell's screen:
/// its view stands in what the shell lays out, and the same action closes it again.
#[test]
fn the_overview_through_the_door_is_the_shells_own_on_its_screen() {
    let _settled = ui::motion::testing::settled();
    let (mut session, _) = session(ShellLayer::Screen);
    until(&mut session, "the preset switch", |session| {
        on_screen(session, "Strips").is_some()
    });
    assert!(
        on_screen(&session, OVERVIEW).is_none(),
        "the desk is at rest"
    );

    session
        .invoke(CHROME, "overview", &Map::new())
        .unwrap_or_else(|error| panic!("the overview opens: {error:?}"));
    until(&mut session, "the shell's overview", |session| {
        on_screen(session, OVERVIEW).is_some()
    });
    assert!(
        on_screen(&session, "Strips").is_none(),
        "the overview stands where the strips stood"
    );

    session
        .invoke(
            CHROME,
            "overview",
            json!({"open": false}).as_object().unwrap(),
        )
        .unwrap_or_else(|error| panic!("the overview closes: {error:?}"));
    until(&mut session, "the strips again", |session| {
        on_screen(session, OVERVIEW).is_none() && on_screen(session, "Strips").is_some()
    });
}

/// On a laptop's display and a phone's, the switch between the presets and the strips under
/// it stand apart, and every note placed on the desk, one strip each, stands on the display
/// once placed: the desk shows what just arrived on it.
#[test]
fn the_preset_switch_and_the_strips_stand_apart_and_each_placed_note_is_on_screen() {
    for (width, height) in [(WIDTH, HEIGHT), (390, 844)] {
        let _settled = ui::motion::testing::settled();
        let (mut session, notes) = session_at(ShellLayer::Screen, width, height);
        until(&mut session, "the preset switch", |session| {
            within(session, "Strips", width, height).is_some()
        });
        for (index, (note, title)) in notes.iter().zip(TITLES).enumerate() {
            place(&mut session, note);
            until(
                &mut session,
                &format!("{title} on screen at {width}x{height}"),
                |session| within(session, title, width, height).is_some(),
            );
            if index > 0 {
                continue;
            }
            // The switch and the first strip under it.
            let first = within(&session, title, width, height).unwrap();
            let switch = preset_switch(&session);
            assert!(
                !intersects(switch, first),
                "at {width}x{height} the preset switch {switch:?} and the first strip {first:?} stand apart"
            );
        }
        // The framed canvas: the switch and the canvas's entry stand apart too.
        press(&mut session, "Frames");
        until(&mut session, "the canvas's entry", |session| {
            within(session, CANVAS_ENTRY, width, height).is_some()
        });
        let entry = within(&session, CANVAS_ENTRY, width, height).unwrap();
        let switch = preset_switch(&session);
        assert!(
            !intersects(switch, entry),
            "at {width}x{height} the preset switch {switch:?} and the canvas's entry {entry:?} stand apart"
        );
    }
}

/// Where the switch between the presets stands: around its three chips.
fn preset_switch(session: &Session) -> (f64, f64, f64, f64) {
    ["Strips", "Frames", "Spaces"]
        .into_iter()
        .filter_map(|chip| session.shell_control(chip))
        .reduce(|(x, y, w, h), (cx, cy, cw, ch)| {
            let (left, top) = (x.min(cx), y.min(cy));
            let (right, bottom) = ((x + w).max(cx + cw), (y + h).max(cy + ch));
            (left, top, right - left, bottom - top)
        })
        .expect("the preset switch stands")
}

/// The chrome's published resource.
const CHROME: &str = "desktop:chrome";

/// The overview's heading: the overview's own, which no other view of the desk shows.
const OVERVIEW: &str = "Overview";

/// Where the shell's control named `name` stands, when the whole of it is on a display `width`
/// by `height` (not only part of it, as [`on_screen`] asks).
fn within(session: &Session, name: &str, width: i32, height: i32) -> Option<(f64, f64, f64, f64)> {
    session.shell_control(name).filter(|&(x, y, w, h)| {
        w > 0.0
            && h > 0.0
            && x >= 0.0
            && y >= 0.0
            && x + w <= f64::from(width)
            && y + h <= f64::from(height)
    })
}

/// Whether two rectangles `(x, y, w, h)` share any area.
fn intersects(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}
