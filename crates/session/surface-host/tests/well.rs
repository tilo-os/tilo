//! A tilolang shell's frames put a well where a place's window goes (shell.md §8): a window maps
//! into the desk's tab model, taking the tab id as its well's key, and stands exactly at the
//! well's rectangle. A well that moves without changing size (a canvas pan) moves its window in
//! the same frame, and with nothing changing no view runs and no client is configured.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

use tilo_compositor::policy::{LayoutPoint, LayoutRect};
use tilo_compositor::{WindowInfo, WindowPolicy as _};

use tilo_surface_host::{WellPolicy, well_policy};
use ui::headless::FrameHarness;
use ui::widget::Column;
use ui::widget::{Element, Map, PinPlace, Wells, WindowWell, window_well};

/// The desk a shell's windows are placed on: a window of 640×480 across a 1280×800 output.
const VIEWPORT: (u32, u32) = (640, 480);

/// A logical length as the policy rounds it onto the desk.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a well's logical pixels, far inside i32"
)]
fn round(value: f64) -> i32 {
    value.round() as i32
}

fn output() -> Vec<LayoutRect> {
    vec![LayoutRect::new((0, 0).into(), (1280, 800).into())]
}

fn window(id: u64, app: &str) -> WindowInfo {
    WindowInfo {
        id,
        app_id: Some(app.into()),
        title: Some(app.into()),
        ..WindowInfo::default()
    }
}

/// A frame as the shell's views build it, laid out with its wells reporting into `wells` — the
/// handle the policy placing its windows shares — for as long as it stands.
struct Frame {
    /// Laid out, and kept alive: the wells hold while it does.
    #[expect(dead_code, reason = "kept only so the frame stays laid out")]
    harness: FrameHarness,
    _wells: window_well::Installed,
}

impl Frame {
    /// The frame the shell draws: a fixed bar of `bar` logical pixels, then the well for `place`
    /// filling the rest of the window.
    fn new(wells: Wells, bar: f32, place: &str, viewport: (u32, u32)) -> Self {
        let frame: Element = Box::new(
            Column::new()
                .with_no_padding()
                .push(Box::new(ui::widget::Spacer::fixed(bar)))
                .push(Box::new(WindowWell::new(place))),
        );
        let mut harness = FrameHarness::new(ui::State::new(), frame, viewport);
        let installed = window_well::install(wells);
        harness.relayout();
        Self {
            harness,
            _wells: installed,
        }
    }
}

/// The window of an app as the session maps it, and the key its well takes: its tab.
fn mapped(policy: &mut WellPolicy, id: u64, app: &str) -> (WindowInfo, String) {
    let window = window(id, app);
    let _ = policy.initial_size(&window, &output());
    let key = policy
        .well_key(id)
        .expect("the desk gave the window its tab");
    (window, key)
}

#[test]
fn a_window_opens_at_exactly_its_wells_rectangle() {
    let _settled = ui::motion::testing::settled();
    let (mut policy, _desk) = well_policy(7, 1.0);
    let outputs = output();
    let (window, key) = mapped(&mut policy, 41, "org.example.App");
    // The shell lays a frame out: its bar of 48 logical pixels over the well of that place.
    let wells = policy.wells();
    let _frame = Frame::new(wells.clone(), 48.0, &key, VIEWPORT);
    let well = wells
        .well(&key)
        .unwrap_or_else(|| panic!("the well of place {key} is on screen"));
    assert!(
        well.y0 >= 48.0 && well.y1 > well.y0 && well.width() > 0.0,
        "the well stands under the frame's bar and takes the room left it: {well:?}"
    );
    // The policy gives the window the desk's tab, and places it exactly there.
    let size = policy
        .initial_size(&window, &outputs)
        .expect("the well's size");
    assert_eq!(
        size,
        (round(well.width()), round(well.height())),
        "the window opens at its well's size"
    );
    let at = policy.place(&window, size, &outputs);
    assert_eq!(
        at,
        LayoutPoint::from((round(well.x0), round(well.y0))),
        "and stands at its well's corner"
    );
    // Laid out again with the well where it is, the client is not configured.
    let arranged = policy
        .arrange(
            &[(window.clone(), LayoutRect::new(at, size.into()))],
            &outputs,
        )
        .expect("an arrangement");
    assert_eq!(arranged.len(), 1);
    let (same, _rest) = arranged.split_first().expect("the window is arranged");
    assert_eq!(same.location, at);
    assert!(
        same.size.is_none(),
        "a well that did not change size leaves the client alone"
    );
}

#[test]
fn a_well_that_moves_moves_its_window_in_the_same_frame_without_running_the_view() {
    let _settled = ui::motion::testing::settled();
    let (mut policy, _desk) = well_policy(7, 1.0);
    let outputs = output();
    let (window, key) = mapped(&mut policy, 42, "org.example.App");
    // A canvas: the place's well at a point on a pannable surface.
    let wells = policy.wells();
    let canvas: Element = Box::new(Map::new().pin(
        "pin",
        PinPlace {
            x: 100.0,
            y: 100.0,
            width: 400.0,
            height: None,
        },
        Box::new(WindowWell::new(key.clone())),
    ));
    let mut harness = FrameHarness::new(ui::State::new(), canvas, VIEWPORT);
    let _installed = window_well::install(wells.clone());
    harness.relayout();
    let before = wells.well(&key).expect("the well is on screen");
    let size = policy.initial_size(&window, &outputs).expect("a size");
    let at = policy.place(&window, size, &outputs);
    assert_eq!(
        at,
        LayoutPoint::from((round(before.x0), round(before.y0))),
        "the window at the pin"
    );
    // The canvas pans: the frame a pan runs moves the well, and the policy places the window
    // where the well stands now — the same frame, with no view run.
    let map = harness
        .root_widget_mut()
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<Map>())
        .expect("the canvas is the root");
    let mut camera = map.camera();
    camera.pan_by(ui::kurbo::Vec2::new(60.0, 40.0));
    map.set_camera(camera);
    // The canvas lays its pins out again from the camera it now holds: a layout pass, not a
    // view run.
    harness.recompose().expect("the canvas lays out");
    let after = wells.well(&key).expect("the well is still on screen");
    assert_ne!(after, before, "the pan moved the well");
    let moved = policy.place(&window, size, &outputs);
    assert_eq!(
        moved,
        LayoutPoint::from((round(after.x0), round(after.y0))),
        "the window moved with its well in the same frame"
    );
    assert!(
        policy
            .arrange(&[(window, LayoutRect::new(at, size.into()))], &outputs)
            .expect("an arrangement")
            .first()
            .expect("the window is arranged")
            .size
            .is_none(),
        "a well that only moved leaves the client's size alone"
    );
}

#[test]
fn a_place_with_no_well_on_screen_waits_off_the_output() {
    let _settled = ui::motion::testing::settled();
    let (mut policy, _desk) = well_policy(7, 1.0);
    let outputs = output();
    let (window, key) = mapped(&mut policy, 43, "org.example.App");
    // The shell lays out a frame for another place: this one's well is not on screen.
    let wells = policy.wells();
    let _frame = Frame::new(wells.clone(), 48.0, "another place", VIEWPORT);
    assert!(wells.well(&key).is_none(), "no well on screen");
    let at = policy.place(&window, (100, 100), &outputs);
    assert!(at.x < 0, "the window waits off the output, at {at:?}");
    let arranged = policy
        .arrange(
            &[(window, LayoutRect::new(at, (100, 100).into()))],
            &outputs,
        )
        .expect("an arrangement");
    let (off, _rest) = arranged.split_first().expect("the window is arranged");
    assert!(off.location.x < 0, "still off the output");
}

#[test]
fn a_well_that_leaves_the_screen_leaves_its_window_off_the_output() {
    let _settled = ui::motion::testing::settled();
    let (mut policy, _desk) = well_policy(7, 1.0);
    let outputs = output();
    let (window, key) = mapped(&mut policy, 44, "org.example.App");
    let wells = policy.wells();
    let frame = Frame::new(wells.clone(), 48.0, &key, VIEWPORT);
    assert!(wells.well(&key).is_some());
    drop(frame);
    assert!(
        wells.well(&key).is_none(),
        "the frame is gone: its well leaves the screen and its window waits off the output"
    );
    let at = policy.place(&window, (100, 100), &outputs);
    assert!(at.x < 0);
}

#[test]
fn with_nothing_changing_no_client_is_configured_and_no_well_reported_twice() {
    let _settled = ui::motion::testing::settled();
    let (mut policy, _desk) = well_policy(7, 1.0);
    let outputs = output();
    let (window, key) = mapped(&mut policy, 45, "org.example.App");
    let wells = policy.wells();
    let frame = Frame::new(wells.clone(), 48.0, &key, VIEWPORT);
    let well = wells.well(&key).expect("the well is on screen");
    let at = LayoutPoint::from((round(well.x0), round(well.y0)));
    let size = (round(well.width()), round(well.height()));
    let windows = [(window, LayoutRect::new(at, size.into()))];
    // The generation moves only when a well reports something new.
    let generation = wells.generation();
    for _ in 0..200 {
        let arranged = policy.arrange(&windows, &outputs).expect("an arrangement");
        assert_eq!(arranged.len(), 1);
        let (laid, _rest) = arranged.split_first().expect("the window is arranged");
        assert_eq!(laid.location, at);
        assert!(
            laid.size.is_none(),
            "a well that did not change size leaves its window's client alone"
        );
    }
    assert_eq!(
        wells.generation(),
        generation,
        "nothing moved, so nothing was reported"
    );
    drop(frame);
}

/// A window whose client rounds what it commits to a grid of cells, as a terminal does: told a
/// size, it draws the largest whole number of cells that fits and commits that. It is the
/// client `well_policy` places, and the size it last committed is what the compositor lists
/// it at.
struct Cells {
    /// The width and height of one cell, in logical pixels.
    cell: (i32, i32),
    /// What it last committed.
    committed: (i32, i32),
}

impl Cells {
    /// A client of `cell` pixels that opens at `asked`.
    fn new(cell: (i32, i32), asked: (i32, i32)) -> Self {
        Self {
            cell,
            committed: (asked.0 / cell.0 * cell.0, asked.1 / cell.1 * cell.1),
        }
    }

    /// The size this client commits when it is told `asked`.
    fn rounded(&self, asked: (i32, i32)) -> (i32, i32) {
        (
            (asked.0 / self.cell.0 * self.cell.0).max(self.cell.0),
            (asked.1 / self.cell.1 * self.cell.1).max(self.cell.1),
        )
    }
}

/// A policy that hands its windows wherever the desk's own towers stand, and records every
/// size it configured a client to: what a click on a window must never add to.
#[test]
fn a_client_that_rounds_its_commit_to_a_cell_grid_is_never_configured_again_by_a_click() {
    let _settled = ui::motion::testing::settled();
    let (mut policy, _desk) = well_policy(7, 1.0);
    let outputs = output();
    let (window, key) = mapped(&mut policy, 46, "org.example.Terminal");
    let wells = policy.wells();
    let _frame = Frame::new(wells.clone(), 48.0, &key, VIEWPORT);
    let well = wells.well(&key).expect("the well is on screen");
    let at = LayoutPoint::from((round(well.x0), round(well.y0)));
    let asked = (round(well.width()), round(well.height()));
    // The client rounds the well's size to its own cells, as a terminal does.
    let mut client = Cells::new((17, 29), asked);
    assert_ne!(
        client.committed, asked,
        "precondition: the client's cells do not divide the well exactly"
    );
    // The window opens: the policy lays it out once, and the client commits its cells.
    let arranged = policy
        .arrange(
            &[(window.clone(), LayoutRect::new(at, client.committed.into()))],
            &outputs,
        )
        .expect("an arrangement");
    if let Some(size) = arranged.first().and_then(|laid| laid.size) {
        client.committed = client.rounded(size);
    }
    // Ten clicks on the window: each one a press and a release, each asking the policy to lay
    // the windows out again (what the compositor does after every frame's input). None of them
    // may configure it: the client committed a size within the room its well gives it, and a
    // click changes no room.
    let mut configured: Vec<(i32, i32)> = Vec::new();
    for _ in 0..10 {
        let current = LayoutRect::new(at, client.committed.into());
        let arranged = policy.arrange(&[(window.clone(), current)], &outputs);
        let arranged = arranged.expect("an arrangement");
        let (laid, _rest) = arranged.split_first().expect("the window is arranged");
        if let Some(size) = laid.size
            && size != client.committed
        {
            configured.push(size);
            client.committed = client.rounded(size);
        }
        assert_eq!(laid.location, at, "the window stays where its well is");
    }
    assert_eq!(
        configured,
        Vec::<(i32, i32)>::new(),
        "a click never configures a client that committed a size within its constraint"
    );
}
