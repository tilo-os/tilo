//! The tilolang shell on every display: one part per output, on its own display's
//! rectangle (each part follows an output added, moved or taken away), not stretched across the
//! displays' bounding box as one; and a part steps aside on the display a fullscreen window owns,
//! from the signal the compositor names those displays by.

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

use serde_json::{Map, Value, json};
use ui::accesskit::{Node, TreeUpdate};

use crate::common::window::Client;
use crate::common::{TITLES, notebook};
use tilo_compositor::{OutputChange, OutputConfig};
use tilo_service::Service;
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

/// A session on `outputs` , each `WIDTH` x `HEIGHT` at its place.
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
    crate::common::wait_for(session, "what the test asked of the session", |session| {
        done(session).then_some(())
    });
}

/// How many passes in a row the shell must run no view and draw no frame to be at rest.
const REST: u32 = 10;

/// Run passes until the shell is at rest: its part has drawn, and over [`REST`] more passes its
/// views ran no turn, it drew no frame and no display owes one. What a display shows then is what
/// it goes on showing until something changes, so a frame read now is the frame the person looks
/// at. The counters are the shell part's own ([`Session::shell_work`]), the ones the shell's idle
/// tests hold it to; a count of passes long enough for a busy machine draws the same frame and
/// then draws it hundreds of times more.
#[track_caller]
fn until_at_rest(session: &mut Session) {
    crate::common::wait_for(session, "the shell to come to rest", |session| {
        let before = session.shell_work()?;
        for _ in 0..REST {
            session.settle_services();
            session.step(Duration::from_millis(1)).unwrap();
        }
        let after = session.shell_work()?;
        (after.drawn > 0
            && (after.turns, after.drawn) == (before.turns, before.drawn)
            && !session.compositor().owes_frame())
        .then_some(())
    });
}

/// Where the part on `output` stands, `(x, y, w, h)` , and whether it stepped aside: `None` while
/// no part stands there.
fn part(session: &Session, output: &str) -> Option<((i32, i32, u32, u32), bool)> {
    session
        .shell_parts()
        .into_iter()
        .find(|part| part.output == output)
        .map(|part| (part.rect, part.aside))
}

tilo_test_env::scenario! {
    fn every_display_has_a_shell_of_its_own_at_its_scale;

    phase every_display_has_a_shell_on_its_own_rectangle;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
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

    phase a_display_added_later_gets_its_own_shell;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
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

    phase a_shell_on_a_scaled_display_draws_at_that_scale;
    // The shell on a `HiDPI` display draws at that display's scale: its part paints the pixels the
    // panel shows, so the bar and the shell are as crisp as the display allows rather than one scale's
    // worth stretched over twice the area.
    {
        let _settled = tilo_ui_base::motion::testing::settled();
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
}

#[test]
fn a_display_turned_off_takes_its_part_and_brings_it_back() {
    let _settled = tilo_ui_base::motion::testing::settled();
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
        "the gone display's part offers nothing on the action API"
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
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = session(&[(0, 0), (WIDTH, 0)]);
    until(&mut session, |session| session.shell_parts().len() == 2);
    let mut client = Client::connect(session.compositor());
    client.pump(&mut session, |_, seen| {
        seen.wm_base.is_some() && seen.shm.is_some()
    });
    let full = client.map_plain(&mut session, "full", (300, 200));
    assert!(
        session.shell_parts().iter().all(|part| !part.aside),
        "a window that is not fullscreen leaves every part in place: {:?}",
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

/// A fractional scale is a scale like any other: a display at 1.75 draws the shell's part at 1.75,
/// its buffer the display's pixels (one and three quarters each way per logical point), and the
/// part's logical rectangle the same logical size a 1 scale would give. A nested session on a
/// desktop panel at 175% — a laptop panel of this size among them — reads that scale from its
/// window and draws the same way.
#[test]
fn a_shell_on_a_fractional_scale_display_draws_at_that_scale() {
    let _settled = tilo_ui_base::motion::testing::settled();
    // The display's mode is its logical size times its scale, as `--size WxH --scale 1.75` names
    // them: a 1600x1000 desktop at 1.75 is a 2800x1750 output.
    let (logical_width, logical_height) = (1600, 1000);
    let scale = 1.75;
    let width = (f64::from(logical_width) * scale).round() as i32;
    let height = (f64::from(logical_height) * scale).round() as i32;
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", width, height).with_scale(scale)],
        options(),
    );
    until(&mut session, |session| !session.shell_parts().is_empty());
    let part = session
        .shell_parts()
        .into_iter()
        .find(|part| part.output == "HEADLESS-1")
        .expect("the display's part");
    assert!(
        (part.scale - scale).abs() < f64::EPSILON,
        "the part draws at 1.75, the display's scale: {part:?}"
    );
    // The part stands on the display's rectangle in the logical layout, which is its mode over its
    // scale: the logical size the person asked for, not the physical pixels.
    assert_eq!(
        (part.rect.2, part.rect.3),
        (
            u32::try_from(logical_width).expect("width"),
            u32::try_from(logical_height).expect("height"),
        ),
        "the part is the logical 1600x1000, not the 2800x1750 pixels: {part:?}"
    );
    // Its buffer is drawn at the display's pixels: one logical point is 1.75 buffer pixels, so the
    // shell is as crisp as the panel rather than a 1600-wide picture stretched over 2800.
    until_at_rest(&mut session);
    session.compositor().render().unwrap();
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a frame");
    assert_eq!(
        (shot.width, shot.height),
        (
            u32::try_from(width).expect("width"),
            u32::try_from(height).expect("height"),
        ),
        "the frame is the display's 2800x1750 pixels"
    );
}

/// The shell's bar sits at the top of the display at every scale: its band starts at the display's
/// very top and covers the same logical height at 2 as at 1, rather than being a band drawn at one
/// scale and stretched over twice the area (which puts its edge where the panel's pixels say not).
#[test]
fn the_shell_bar_band_sits_at_the_top_of_the_display_at_every_scale() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut bands = Vec::new();
    for (scale, pixels) in [(1.0, 1), (2.0, 2)] {
        let mut session = crate::common::headless_on(
            &[OutputConfig::new("HEADLESS-1", WIDTH * pixels, HEIGHT * pixels).with_scale(scale)],
            Options {
                person: "Pat".to_owned(),
                ..Options::default()
            },
        );
        // The bar's band is read off the frame the shell comes to rest on.
        until_at_rest(&mut session);
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

/// A bar flush with the display's edges paints them in its own colour: its top row and its first
/// and last columns are the body's colour at 2, with no edge highlight along the screen's sides.
/// The theme's glass body lights a rim along its outline. On a bar edge to edge with the screen
/// that rim fell on the display's own top row and end columns, reading as a glass line between the
/// bar and the bezel rather than as the bar reaching the edge.
#[test]
fn a_bar_flush_with_the_display_paints_its_edges_its_own_colour() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let width = WIDTH * 2;
    let height = HEIGHT * 2;
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", width, height).with_scale(2.0)],
        Options {
            person: "Pat".to_owned(),
            collab: false,
            ..Options::default()
        },
    );
    until(&mut session, |session| !session.shell_parts().is_empty());
    // The translucent style is where the glass rim shows.
    let before = session.shell_work().expect("the shell runs");
    session
        .run_shell_action("set_bar_finish", vec![serde_json::json!("translucent")])
        .expect("the bar takes the theme's glass");
    // The bar stands in its glass once the shell has drawn since it took the finish, and has come
    // to rest on that frame.
    until(&mut session, |session| {
        session
            .shell_work()
            .is_some_and(|now| now.drawn > before.drawn)
    });
    until_at_rest(&mut session);
    session.compositor().render().unwrap();
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a frame");
    // The glass body lights a rim one pixel thick along its outline. On a bar edge to edge with the
    // screen that rim lands on the display's own top row and end columns: compare each edge pixel
    // with the one just inside it, so the material's own sheen — a smooth gradient across the body
    // — is not read as an edge, and a one-pixel line of light is.
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

// --- The displays a shell's strips and bar are each drawn on -------------------------------------------------

/// A shell session on `places` (each `WIDTH` x `HEIGHT`), hosting `services`.
fn shell_on(places: &[(i32, i32)], services: Vec<Box<dyn Service>>) -> Session {
    let outputs: Vec<OutputConfig> = places
        .iter()
        .enumerate()
        .map(|(index, place)| {
            OutputConfig::new(format!("HEADLESS-{}", index + 1), WIDTH, HEIGHT).at(place.0, place.1)
        })
        .collect();
    let mut session = crate::common::headless_on(
        &outputs,
        Options {
            services,
            ..options()
        },
    );
    session.step(Duration::from_millis(1)).unwrap();
    session
}

/// Place `resource` as a window of its own in the workspace in front, through the layout's
/// published `place`.
fn place_note(session: &mut Session, resource: &str) -> u64 {
    let id = session.handle().mint_id();
    let (workspace, row, strip) = {
        let tree = session.handle().read(Clone::clone);
        let workspace = tree.active_workspace().expect("a workspace is in front");
        (
            workspace.id().to_string(),
            workspace.strips.first().expect("a row").id.0,
            session.handle().mint_id(),
        )
    };
    let arguments: Map<String, Value> = json!({
        "workspace": workspace,
        "row": row,
        "strip": strip,
        "place": id,
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
    id
}

/// The windows every part of the shell exports, each with the display it stands on.
fn windows(session: &Session) -> Vec<(usize, String, TreeUpdate)> {
    session.a11y_windows()
}

/// The trees the shell's parts export, one per display: what a screen reader reads of each
/// display's page, which holds that display's strips, bar and windows.
fn shell_trees(session: &Session) -> Vec<TreeUpdate> {
    windows(session)
        .into_iter()
        .filter(|(_, name, _)| name == "Desktop shell")
        .map(|(_, _, tree)| tree)
        .collect()
}

/// Whether the exported tree `tree` says `text` in one of its labels: a window's name, a
/// control's words.
fn tree_says(tree: &TreeUpdate, text: &str) -> bool {
    tree.nodes
        .iter()
        .any(|(_, node)| node.label().is_some_and(|label| label.contains(text)))
}

/// The shell part that exports the window `name`: its index among the shell's parts, waited for
/// until some part says it. A part whose page has not run yet exports nothing of the window.
fn part_showing(session: &mut Session, name: &str) -> usize {
    crate::common::steps_within(session, Duration::from_secs(60), |session| {
        shell_trees(session)
            .iter()
            .position(|tree| tree_says(tree, name))
    })
    .unwrap_or_else(|| panic!("no display's part draws {name}: {:?}", shell_trees(session)))
}

/// The words the part on `output` shows: what each element's semantics name, say or describe.
/// What a display's part draws, read from that part alone.
fn part_words(session: &Session, output: &str) -> Vec<String> {
    fn walk(node: &ui::composition::Composed, out: &mut Vec<String>) {
        if let Some(semantics) = &node.semantics {
            for said in semantics.nodes() {
                out.extend(said.name.iter().cloned());
                out.extend(said.value.iter().cloned());
                out.extend(said.description.iter().cloned());
            }
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let Some(composition) = session.shell_composition(output) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk(&composition.root, &mut out);
    out
}

/// The words the part on `output` composes now.
fn words_on(session: &Session, output: &str) -> Vec<String> {
    part_words(session, output)
}

/// The node numbered `id` in `tree`.
fn root(tree: &TreeUpdate) -> &Node {
    let root = tree.tree.as_ref().expect("a window names its root").root;
    tree.nodes
        .iter()
        .find(|(id, _)| *id == root)
        .map_or_else(|| panic!("the window exports its root"), |(_, node)| node)
}

/// The id of the node in `tree` labelled exactly `label`.
fn labelled(tree: &TreeUpdate, label: &str) -> Option<ui::accesskit::NodeId> {
    tree.nodes
        .iter()
        .find(|(_, node)| node.label() == Some(label))
        .map(|(id, _)| *id)
}

/// Move the window in front to the next display, through the shell's published action.
fn move_to_display(session: &mut Session, steps: i64) {
    let arguments: Map<String, Value> = json!({ "steps": steps }).as_object().unwrap().clone();
    session
        .invoke("desktop:shell", "move-to-display", &arguments)
        .unwrap_or_else(|error| panic!("move-to-display: {error:?}"));
}

/// Whether the desktop lays the device out across several displays, each showing its own strip.
fn partitioned(session: &Session) -> bool {
    session
        .handle()
        .read(tilo_desktop_model::Layout::is_partitioned)
}

/// The window group holding the place `place`, as this device's model holds it.
fn group_of(session: &Session, place: u64) -> Option<tilo_desktop_model::GroupId> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .find(|group| group.tabs.iter().any(|tab| tab.id == place))
            .map(|group| group.id)
    })
}

/// The display the place `place` stands on in this device's model: where the desktop put it,
/// whether or not a shell view has run since.
fn shown_on(session: &Session, place: u64) -> Option<i64> {
    let group = group_of(session, place)?;
    session
        .handle()
        .read(|tree| tree.outputs.display_of(group))
        .map(i64::from)
}

/// The place the shell's next display action is about: the active tab of the strip the keyboard
/// is in.
fn active_place(session: &Session) -> Option<u64> {
    session.handle().read(|tree| {
        tilo_session_core::desktop::navigation::active_route(tree).map(|route| route.tab)
    })
}

/// Whether every place of the layout stands on a display: the session's own pass that reconciles
/// the partition has placed them.
fn homed(session: &Session, places: usize) -> bool {
    session
        .handle()
        .read(|tree| tree.outputs.home.len() >= places)
}

/// The display the person made primary, by its id in this device's layout.
fn primary_display(session: &Session) -> Option<i64> {
    session
        .handle()
        .read(|tree| tree.outputs.primary())
        .map(i64::from)
}

/// The connector of the output showing display `display` of the layout.
fn output_of(session: &mut Session, display: i64) -> String {
    let id = u32::try_from(display).expect("a display id");
    let monitor = session
        .handle()
        .read(|tree| tree.outputs.monitor_of(id).map(str::to_owned))
        .expect("the display names its monitor");
    session
        .compositor()
        .outputs()
        .into_iter()
        .find(|output| output.monitor == monitor)
        .map(|output| output.name)
        .expect("a compositor output holds the monitor")
}

/// Carry the place `place` to the other display, through the shell's own move action: a window
/// put on the layout lands on the display in use, so a test that wants a strip on each display
/// moves one. The fixtures put HEADLESS-1 left of HEADLESS-2, so stepping away from the first
/// display goes right and away from the last goes left.
fn move_to(session: &mut Session, place: u64) {
    let here = shown_on(session, place).unwrap_or(0);
    let steps = if here == 0 { 1 } else { -1 };
    move_to_display(session, steps);
    until(session, |session| shown_on(session, place) != Some(here));
}

#[test]
fn each_display_draws_its_own_strip_and_only_the_primary_carries_the_whole_bar() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (notebook, titles) = notebook(&TITLES[..2]);
    let mut session = shell_on(&[(0, 0), (WIDTH, 0)], vec![Box::new(notebook)]);
    until(&mut session, |session| session.shell_parts().len() == 2);
    until(&mut session, |session| partitioned(session));
    let first = place_note(&mut session, &titles[0]);
    let second = place_note(&mut session, &titles[1]);
    until(&mut session, |session| {
        crate::common::places(session)
            .iter()
            .any(|place| place.resource == titles[1])
    });
    // The move action moves the window in front: the one just placed, once the session's pass
    // has given every place its display.
    until(&mut session, |session| homed(session, 2));
    until(&mut session, |session| {
        active_place(session) == Some(second)
    });
    // Each place stands in a window of its own, so moving one leaves the other where it is.
    assert_ne!(
        group_of(&session, first),
        group_of(&session, second),
        "each place stands in a window of its own: {:?}",
        crate::common::layout(&session).strips
    );
    // The person puts windows on the layout and they land on the display in use; taking one to
    // the other display leaves the other where it was and gives each display a strip.
    until(&mut session, |session| partitioned(session));
    let before_first = shown_on(&session, first).expect("the first window's display");
    let before = session.handle().read(|tree| {
        (
            tree.outputs.focused.clone(),
            tree.outputs.primary(),
            tree.outputs.home.clone(),
            tree.outputs.displays.clone(),
        )
    });
    move_to(&mut session, second);
    let first_display = shown_on(&session, first).expect("the first window's display");
    let second_display = shown_on(&session, second).expect("the second window's display");
    assert_eq!(
        first_display,
        before_first,
        "the window the person did not move stays where it was: {:?} before {:?} now {:?}",
        crate::common::layout(&session).strips,
        before,
        session.handle().read(|tree| (
            tree.outputs.focused.clone(),
            tree.outputs.primary(),
            tree.outputs.home.clone(),
            tree.outputs.displays.clone(),
        ))
    );
    assert_ne!(
        first_display,
        second_display,
        "the moved window stands on the other display: {:?} {:?}",
        crate::common::layout(&session).strips,
        session.handle().read(|tree| (
            tree.outputs.focused.clone(),
            tree.outputs.primary(),
            tree.outputs.home.clone(),
        ))
    );
    let shown_first = crate::common::drawn_name(&session, &titles[0]);
    let shown_second = crate::common::drawn_name(&session, &titles[1]);
    // Each display's part says its own strip's window and no other's, as a screen reader reads
    // each display's page.
    part_showing(&mut session, &shown_first);
    part_showing(&mut session, &shown_second);
    let trees = shell_trees(&session);
    let part_of = |name: &str| {
        trees
            .iter()
            .position(|tree| tree_says(tree, name))
            .unwrap_or_else(|| panic!("no display's part says {name}"))
    };
    let first_part = part_of(&shown_first);
    let second_part = part_of(&shown_second);
    assert_ne!(
        first_part, second_part,
        "each display's part draws its own window: {shown_first:?} {shown_second:?}"
    );
    assert!(
        !tree_says(&trees[first_part], &shown_second),
        "the first display draws no other's window: {shown_first:?} {shown_second:?}"
    );
    assert!(
        !tree_says(&trees[second_part], &shown_first),
        "the second display draws no other's window: {shown_first:?} {shown_second:?}"
    );
    // The primary display carries the whole bar (its bell is there); the other carries the
    // compact bar, which keeps the time, the workspace and the strip and gives the rest up.
    let primary = primary_display(&session).expect("a display is primary");
    let (whole, compact) = if first_display == primary {
        (first_part, second_part)
    } else {
        (second_part, first_part)
    };
    assert!(
        tree_says(&trees[whole], "Notifications"),
        "the primary display carries the whole bar: {}",
        root(&trees[whole]).label().unwrap_or_default()
    );
    assert!(
        !tree_says(&trees[compact], "Notifications"),
        "the other display carries the compact bar, with no bell"
    );
}

#[test]
fn unplugging_a_display_keeps_its_windows_on_the_one_left() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (notebook, titles) = notebook(&TITLES[..2]);
    let mut session = shell_on(&[(0, 0), (WIDTH, 0)], vec![Box::new(notebook)]);
    until(&mut session, |session| session.shell_parts().len() == 2);
    until(&mut session, |session| partitioned(session));
    let first = place_note(&mut session, &titles[0]);
    let second = place_note(&mut session, &titles[1]);
    until(&mut session, |session| {
        crate::common::places(session)
            .iter()
            .any(|place| place.resource == titles[1])
    });
    until(&mut session, |session| homed(session, 2));
    until(&mut session, |session| {
        active_place(session) == Some(second)
    });
    move_to(&mut session, second);
    let apart = shown_on(&session, second).expect("the moved window's display");
    let gone = output_of(&mut session, apart);
    let shown_first = crate::common::drawn_name(&session, &titles[0]);
    let shown_second = crate::common::drawn_name(&session, &titles[1]);

    session
        .compositor()
        .configure_outputs(&[OutputChange::disable(gone.clone())])
        .unwrap();
    until(&mut session, |session| session.shell_parts().len() == 1);
    assert_eq!(
        shown_on(&session, second),
        shown_on(&session, first),
        "the window on the display that went moves to the one left"
    );
    assert!(
        crate::common::place(&session, second).is_some()
            && crate::common::place(&session, first).is_some(),
        "no window is lost with its display"
    );
    part_showing(&mut session, &shown_first);
    part_showing(&mut session, &shown_second);
    let left = shell_trees(&session);
    assert_eq!(left.len(), 1, "one display is left: {}", left.len());
    assert!(
        tree_says(&left[0], &shown_first) && tree_says(&left[0], &shown_second),
        "the display left draws both windows: {shown_first:?} {shown_second:?}"
    );

    // The display comes back: the window that left it returns to it, not to the one in use.
    session
        .compositor()
        .configure_outputs(&[OutputChange::enable(gone)])
        .unwrap();
    until(&mut session, |session| session.shell_parts().len() == 2);
    until(&mut session, |session| {
        shown_on(session, second) != shown_on(session, first)
    });
}

#[test]
fn a_part_made_before_a_layout_reads_it_too() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (notebook, titles) = notebook(&TITLES[..2]);
    let mut session = shell_on(&[(0, 0)], vec![Box::new(notebook)]);
    until(&mut session, |session| session.shell_parts().len() == 1);
    place_note(&mut session, &titles[0]);
    until(&mut session, |session| {
        crate::common::places(session)
            .iter()
            .any(|place| place.resource == titles[0])
    });
    // The second display's part is made before the layout changes again: it must read the
    // layout the first part applied, not miss it because the first poll took it.
    let mut output = OutputConfig::new("HEADLESS-2", WIDTH, HEIGHT);
    output.position = Some((WIDTH, 0));
    session.compositor().add_output(&output).unwrap();
    until(&mut session, |session| session.shell_parts().len() == 2);
    until(&mut session, |session| partitioned(session));
    let second = place_note(&mut session, &titles[1]);
    until(&mut session, |session| {
        crate::common::places(session)
            .iter()
            .any(|place| place.resource == titles[1])
    });
    until(&mut session, |session| homed(session, 2));
    until(&mut session, |session| {
        active_place(session) == Some(second)
    });
    move_to(&mut session, second);
    let shown = crate::common::drawn_name(&session, &titles[1]);
    let first_shown = crate::common::drawn_name(&session, &titles[0]);
    part_showing(&mut session, &shown);
    part_showing(&mut session, &first_shown);
    let trees = shell_trees(&session);
    let moved = trees
        .iter()
        .position(|tree| tree_says(tree, &shown))
        .unwrap_or_else(|| panic!("no display's part draws the window that moved: {shown:?}"));
    let first = trees
        .iter()
        .position(|tree| tree_says(tree, &first_shown))
        .unwrap_or_else(|| panic!("no display's part draws the first window: {first_shown:?}"));
    assert_ne!(
        moved, first,
        "the part made before the layout reads it and draws the window that moved: {shown:?}"
    );
}

#[test]
fn the_shell_is_the_window_its_view_names() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = shell_on(&[(0, 0)], Vec::new());
    until(&mut session, |session| session.shell_parts().len() == 1);
    until(&mut session, |session| {
        windows(session)
            .iter()
            .any(|(_, name, _)| name == "Desktop shell")
    });
    let (_, name, tree) = windows(&session)
        .into_iter()
        .find(|(_, name, _)| name == "Desktop shell")
        .expect("the shell's window");
    assert_eq!(
        name, "Desktop shell",
        "the view names the window a screen reader reads"
    );
    assert_eq!(root(&tree).label(), Some("Desktop shell"));
}

#[test]
fn a_view_marks_the_unread_count_as_a_polite_live_region() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = shell_on(&[(0, 0)], Vec::new());
    until(&mut session, |session| {
        session.shell_control("Notifications").is_some()
    });
    session.notices().post(
        tilo_session_core::desktop::notifications::Notification::new(
            "Backup finished",
            "12 GB safe",
        )
        .with_app_name("Settings"),
    );
    // The bell says what is waiting, and the centre is opened as a person presses it.
    until(&mut session, |session| {
        session.shell_control("Notifications, 1 unread").is_some()
    });
    // The bell is pressed through the export, as a screen reader presses it.
    until(&mut session, |session| {
        windows(session).iter().any(|(_, name, tree)| {
            name == "Desktop shell" && labelled(tree, "Notifications, 1 unread").is_some()
        })
    });
    let (place, tree) = windows(&session)
        .into_iter()
        .find(|(_, name, _)| name == "Desktop shell")
        .map(|(place, _, tree)| (place, tree))
        .expect("the shell's window");
    let bell = labelled(&tree, "Notifications, 1 unread").expect("the bell names what is waiting");
    assert!(
        session.a11y_perform(place, bell, &tilo_ui_base::a11y::Action::Activate),
        "the export takes the activation"
    );
    let waiting =
        |tree: &TreeUpdate| labelled(tree, "All read").or_else(|| labelled(tree, "1 unread"));
    until(&mut session, |session| {
        windows(session)
            .iter()
            .any(|(_, _, tree)| waiting(tree).is_some())
    });
    let windows = windows(&session);
    let (_, _, tree) = windows
        .iter()
        .find(|(_, _, tree)| waiting(tree).is_some())
        .expect("the centre's header");
    // The header's own line, by its whole words: the bell also says "unread".
    let line = labelled(tree, "All read").or_else(|| labelled(tree, "1 unread"));
    let line = line.expect("the centre says how many are waiting");
    let (_, node) = tree
        .nodes
        .iter()
        .find(|(id, _)| *id == line)
        .expect("the unread line");
    assert_eq!(
        node.live(),
        Some(ui::accesskit::Live::Polite),
        "the count of what is waiting is announced politely, as the view marks it"
    );
}

#[test]
fn an_arrangement_from_another_device_reaches_every_part() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (notebook, titles) = notebook(&TITLES[..2]);
    let mut session = shell_on(&[(0, 0), (WIDTH, 0)], vec![Box::new(notebook)]);
    until(&mut session, |session| session.shell_parts().len() == 2);
    until(&mut session, |session| partitioned(session));
    let first = place_note(&mut session, &titles[0]);
    let second = place_note(&mut session, &titles[1]);
    until(&mut session, |session| {
        crate::common::places(session)
            .iter()
            .any(|place| place.resource == titles[1])
    });
    until(&mut session, |session| homed(session, 2));
    until(&mut session, |session| {
        active_place(session) == Some(second)
    });
    move_to(&mut session, second);
    let (was_first, was_second) = (shown_on(&session, first), shown_on(&session, second));
    assert_ne!(
        was_first, was_second,
        "each window has a display of its own"
    );
    let shown_first = crate::common::drawn_name(&session, &titles[0]);
    let shown_second = crate::common::drawn_name(&session, &titles[1]);

    // The arrangement another device's Settings makes: the second display is placed left of the
    // first. Each part follows its own display's rectangle, and each window stays where the
    // person put it.
    session
        .compositor()
        .configure_outputs(&[OutputChange {
            position: Some((-WIDTH, 0)),
            ..OutputChange::enable("HEADLESS-2")
        }])
        .unwrap();
    until(&mut session, |session| {
        part(session, "HEADLESS-2").is_some_and(|(rect, _)| rect.0 == -WIDTH)
    });
    assert_eq!(
        shown_on(&session, first),
        was_first,
        "the first window stays on its display"
    );
    assert_eq!(
        shown_on(&session, second),
        was_second,
        "the second window stays on its display"
    );
    part_showing(&mut session, &shown_first);
    part_showing(&mut session, &shown_second);
    let trees = shell_trees(&session);
    let first_part = trees
        .iter()
        .position(|tree| tree_says(tree, &shown_first))
        .unwrap_or_else(|| panic!("no display's part draws the first window: {shown_first:?}"));
    let second_part = trees
        .iter()
        .position(|tree| tree_says(tree, &shown_second))
        .unwrap_or_else(|| panic!("no display's part draws the second window: {shown_second:?}"));
    assert_ne!(
        first_part, second_part,
        "each part draws its own display's window: {shown_first:?} {shown_second:?}"
    );
}

/// The number and name Settings gives each display, by the connector the compositor knows it by:
/// the same pair Settings' placement canvas shows, read the way the session reads it.
fn settings_numbers(session: &mut Session) -> Vec<(String, String, String)> {
    let snapshot = session
        .compositor()
        .display_snapshot()
        .expect("the compositor published its displays");
    let numbers = settings_store::display_identify::display_numbers(&snapshot.outputs);
    let names = settings_store::display_identify::display_names(&snapshot.outputs);
    snapshot
        .outputs
        .iter()
        .zip(numbers)
        .zip(names)
        .map(|((output, number), name)| {
            (
                output.descriptor.connector.clone(),
                number.to_string(),
                name,
            )
        })
        .collect()
}

/// Whether the words `words` say `text` of their own.
fn holds(words: &[String], text: &str) -> bool {
    words.iter().any(|word| word == text)
}

#[test]
fn identifying_the_displays_says_each_one_its_own_number() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = shell_on(&[(0, 0), (WIDTH, 0)], Vec::new());
    until(&mut session, |session| session.shell_parts().len() == 2);
    until(&mut session, |session| partitioned(session));
    until(&mut session, |session| {
        session.shell_control("Notifications").is_some()
    });
    let shown = settings_numbers(&mut session);
    assert_eq!(shown.len(), 2, "each display Settings numbers: {shown:?}");
    let first_output = "HEADLESS-1".to_owned();
    let second_output = "HEADLESS-2".to_owned();
    let card = |output: &str| {
        shown
            .iter()
            .find(|(connector, _, _)| connector == output)
            .map_or_else(
                || panic!("Settings numbers {output}: {shown:?}"),
                |(_, number, name)| (number.clone(), name.clone()),
            )
    };
    let (first_number, first_name) = card(&first_output);
    let (second_number, second_name) = card(&second_output);
    assert_ne!(
        first_number, second_number,
        "each display has a number of its own"
    );

    // Nothing is said before the person asks.
    let before = words_on(&session, &first_output);
    assert!(
        !holds(&before, &first_number),
        "a display says nothing until Settings asks: {before:?}"
    );

    // Settings asks which screen is which: each display says its own number and name.
    settings_store::display_identify::IdentifyRequest::request().expect("identify is asked");
    let first_words =
        crate::common::steps_within(&mut session, Duration::from_secs(60), |session| {
            let words = words_on(session, &first_output);
            holds(&words, &first_number).then_some(words)
        })
        .unwrap_or_else(|| words_on(&session, &first_output));
    let second_words =
        crate::common::steps_within(&mut session, Duration::from_secs(60), |session| {
            let words = words_on(session, &second_output);
            holds(&words, &second_number).then_some(words)
        })
        .unwrap_or_else(|| words_on(&session, &second_output));
    assert!(
        holds(&first_words, &first_number) && holds(&first_words, &first_name),
        "the first display says its own number and name: {first_words:?}"
    );
    assert!(
        holds(&second_words, &second_number) && holds(&second_words, &second_name),
        "the second display says its own number and name: {second_words:?}"
    );
    assert!(
        !holds(&second_words, &first_number) && !holds(&first_words, &second_number),
        "a display says only its own number: {first_words:?} {second_words:?}"
    );
}
