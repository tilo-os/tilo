//! Working on the canvas with the pointer and keys, as a person does: every
//! gesture lands as one of the artwork's actions, one step of history.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::too_many_lines,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::model::{Mark, Shape};
use canvas::{App, Artwork};
use ui::prelude::{Invoke, TestApp};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};

/// The app with the vector sample drawn, the canvas focused, and where the
/// artwork lies on screen.
struct Desk {
    app: TestApp<App>,
    origin: (f64, f64),
    zoom: f64,
    canvas: ui::vello::kurbo::Rect,
    /// Where screenshots go; `CANVAS_SHOTS` when set.
    shots: Option<std::path::PathBuf>,
}

impl Desk {
    fn open() -> Self {
        let mut app = TestApp::<App>::new().expect("the app opens");
        for action in canvas::sample::shapes() {
            app.invoke(Invoke::apply(&action).unwrap())
                .expect("the sample draws");
        }
        let canvas = app
            .harness()
            .targets()
            .get("canvas")
            .copied()
            .expect("the canvas");
        app.harness()
            .perform_named("canvas", &ui::a11y::Action::Focus)
            .expect("the canvas takes focus");
        // The artwork is fitted into the canvas less its 24-point padding.
        let art = app.state();
        let zoom = ((canvas.width() - 48.0) / art.width).min((canvas.height() - 48.0) / art.height);
        // Centred in the canvas.
        let origin = (
            canvas.center().x - art.width * zoom / 2.0,
            canvas.center().y - art.height * zoom / 2.0,
        );
        let mut desk = Self {
            app,
            origin,
            zoom,
            canvas,
            shots: std::env::var_os("CANVAS_SHOTS").map(Into::into),
        };
        desk.key("v");
        desk
    }

    fn at(&self, x: f64, y: f64) -> (f64, f64) {
        (self.origin.0 + x * self.zoom, self.origin.1 + y * self.zoom)
    }

    fn art(&self) -> Artwork {
        self.app.state()
    }

    fn mark(&self, id: &str) -> Mark {
        self.art().mark(id).cloned().expect("the mark is there")
    }

    fn key(&mut self, key: &str) {
        self.app.harness().key(key);
    }

    fn hold(&mut self, keys: &[&str]) {
        let keys: Vec<String> = keys.iter().map(|key| (*key).to_owned()).collect();
        self.app.harness().set_modifiers(&keys);
    }

    fn click(&mut self, x: f64, y: f64) {
        let (x, y) = self.at(x, y);
        self.app.harness().click_at(x, y);
    }

    fn drag(&mut self, from: (f64, f64), to: (f64, f64)) {
        let (from, to) = (self.at(from.0, from.1), self.at(to.0, to.1));
        self.app.harness().drag_at(from, to);
    }

    fn pointer(&mut self, x: f64, y: f64) {
        let (x, y) = self.at(x, y);
        self.app.harness().send(&WindowEvent::CursorMoved {
            device_id: ui::winit::event::DeviceId::dummy(),
            position: ui::winit::dpi::PhysicalPosition::new(x, y),
        });
    }

    fn button(&mut self, state: ElementState) {
        self.app.harness().send(&WindowEvent::MouseInput {
            device_id: ui::winit::event::DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        });
    }

    /// Press at `from` and move through `path` without letting go.
    fn hold_drag(&mut self, from: (f64, f64), path: &[(f64, f64)]) {
        self.pointer(from.0, from.1);
        self.button(ElementState::Pressed);
        for (x, y) in path {
            self.pointer(*x, *y);
        }
    }

    /// Write the window as a person sees it, when screenshots are asked for
    /// (`CANVAS_SHOTS=<dir>`).
    fn shot(&mut self, name: &str) {
        let Some(dir) = self.shots.clone() else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        self.app.harness().set_theme(ui::Theme::light());
        self.app
            .harness()
            .render_window_png(&dir.join(format!("{name}.png")));
    }
}

fn rect_of(mark: &Mark) -> (f64, f64, f64, f64) {
    let rect = match mark.shape {
        Shape::Rect {
            x,
            y,
            width,
            height,
            ..
        } => Some((x, y, width, height)),
        _ => None,
    };
    rect.expect("a rect")
}

fn near(a: f64, b: f64, within: f64) -> bool {
    (a - b).abs() <= within
}

/// A drag moves the selection as one edit, one undo step; dropped near
/// another edge it lands on it.
#[test]
fn a_drag_moves_the_selection_once_and_snaps_to_edges() {
    let mut desk = Desk::open();
    desk.hold(&["ctrl"]);
    desk.drag((270.0, 240.0), (370.0, 290.0));
    desk.hold(&[]);
    let (x, y, ..) = rect_of(&desk.mark("card"));
    assert!(near(x, 220.0, 0.5) && near(y, 190.0, 0.5), "{x} {y}");
    assert!(desk.app.undo(), "the move is one step");
    assert_eq!(rect_of(&desk.mark("card")).0, 120.0);

    // Its outer edge (outline included) dropped half a unit from the
    // artwork's: it snaps onto it.
    desk.drag((270.0, 240.0), (152.0, 240.0));
    let (x, ..) = rect_of(&desk.mark("card"));
    assert!(near(x, 1.5, 1e-6), "{x}");
}

/// A corner stretches freely, keeps the aspect with Shift, and a stretched
/// rectangle stays a rectangle.
#[test]
fn a_corner_stretches_and_shift_keeps_the_aspect() {
    let mut desk = Desk::open();
    desk.click(270.0, 240.0);
    desk.hold(&["ctrl"]);
    desk.drag((420.0, 340.0), (520.0, 360.0));
    desk.hold(&[]);
    let (x, y, width, height) = rect_of(&desk.mark("card"));
    assert!(near(x, 120.0, 0.5) && near(y, 140.0, 0.5));
    assert!(
        near(width, 400.0, 1.0) && near(height, 220.0, 1.0),
        "{width} {height}"
    );
    assert!(
        desk.mark("card").transform.is_none(),
        "baked into the geometry"
    );

    desk.hold(&["shift"]);
    desk.drag((520.0, 360.0), (600.0, 360.0));
    desk.hold(&[]);
    let (_, _, width2, height2) = rect_of(&desk.mark("card"));
    assert!(
        near(width2 / height2, width / height, 0.01),
        "{width2} {height2}"
    );
}

/// The rotation handle turns about the centre, in steps of 15 with Shift.
#[test]
fn the_rotation_handle_turns_in_steps_with_shift() {
    let mut desk = Desk::open();
    desk.click(270.0, 240.0);
    // The rotation handle stands one pointer target above the top centre.
    let above = 32.0 / desk.zoom;
    desk.hold(&["shift"]);
    desk.drag((270.0, 140.0 - above), (450.0, 250.0));
    desk.hold(&[]);
    let [a, b, ..] = desk.mark("card").transform.expect("turned");
    let degrees = b.atan2(a).to_degrees();
    assert!(
        near(
            degrees
                .rem_euclid(15.0)
                .min(15.0 - degrees.rem_euclid(15.0)),
            0.0,
            1e-6
        ),
        "{degrees}"
    );
    assert!(degrees.abs() > 30.0, "{degrees}");
}

/// A marquee picks what it touches, Shift-click adds and takes away, and
/// Delete removes the picked marks in one step.
#[test]
fn marquee_and_shift_click_pick_and_delete_removes() {
    let mut desk = Desk::open();
    desk.drag((60.0, 100.0), (450.0, 380.0));
    desk.hold(&["shift"]);
    desk.click(760.0, 600.0);
    desk.hold(&[]);
    let before = desk.art().marks.len();
    desk.key("Delete");
    let art = desk.art();
    assert_eq!(art.marks.len(), before - 2);
    assert!(art.mark("card").is_none() && art.mark("leaf").is_none());
    assert!(desk.app.undo());
    assert_eq!(desk.art().marks.len(), before);
}

/// A click picks the whole group; a double click picks inside it.
#[test]
fn groups_are_picked_whole_and_grouping_is_one_edit() {
    let mut desk = Desk::open();
    desk.click(270.0, 240.0);
    desk.hold(&["shift"]);
    desk.click(760.0, 600.0);
    desk.hold(&["ctrl"]);
    desk.key("g");
    desk.hold(&[]);
    let group = desk.mark("card").group;
    assert!(!group.is_empty());
    assert_eq!(desk.mark("leaf").group, group);
    // Arrows nudge the whole selection, Shift ten at a time.
    desk.hold(&["shift"]);
    desk.key("ArrowRight");
    desk.hold(&[]);
    assert_eq!(rect_of(&desk.mark("card")).0, 130.0);
    // Now a click on one picks both.
    desk.key("Escape");
    desk.click(280.0, 240.0);
    desk.key("ArrowDown");
    assert_eq!(rect_of(&desk.mark("card")).1, 141.0);
    assert!(desk.app.undo());
    desk.hold(&["ctrl", "shift"]);
    desk.key("g");
    desk.hold(&[]);
    assert!(desk.mark("leaf").group.is_empty());
}

/// Duplicate, copy and paste make new marks beside the old in one step
/// each.
#[test]
fn duplicate_copy_and_paste_make_new_marks() {
    let mut desk = Desk::open();
    let before = desk.art().marks.len();
    desk.click(860.0, 230.0);
    desk.hold(&["ctrl"]);
    desk.key("d");
    desk.hold(&[]);
    let art = desk.art();
    assert_eq!(art.marks.len(), before + 7, "the whole flower");
    let copies: Vec<&Mark> = art
        .marks
        .iter()
        .filter(|mark| mark.id.starts_with("m-"))
        .collect();
    assert_eq!(copies.len(), 7);
    let group = &copies[0].group;
    assert!(!group.is_empty() && group != "flower");
    assert!(copies.iter().all(|mark| mark.group == *group));
    assert!(desk.app.undo(), "a duplicate is one step");
    assert_eq!(desk.art().marks.len(), before);

    desk.click(270.0, 240.0);
    desk.hold(&["ctrl"]);
    desk.key("c");
    desk.key("v");
    desk.key("v");
    desk.hold(&[]);
    let art = desk.art();
    assert_eq!(art.marks.len(), before + 2);
    let pasted: Vec<f64> = art
        .marks
        .iter()
        .filter(|mark| mark.id.starts_with("m-"))
        .map(|mark| rect_of(mark).0)
        .collect();
    assert_eq!(pasted.len(), 2);
    assert!(pasted[0] > 120.0 && pasted[1] > pasted[0], "{pasted:?}");
}

/// The pen lays a corner with a click, a curve with a drag, and closes on
/// its first point: one `draw` of a closed bezier path.
#[test]
fn the_pen_draws_a_closed_bezier_path() {
    let mut desk = Desk::open();
    let before = desk.art().marks.len();
    desk.key("p");
    desk.click(500.0, 400.0);
    desk.drag((700.0, 400.0), (760.0, 460.0));
    desk.click(600.0, 560.0);
    desk.shot("pen-drawing");
    desk.click(500.0, 400.0);
    let art = desk.art();
    assert_eq!(art.marks.len(), before + 1);
    let path = art.marks.last().unwrap();
    let Shape::Path { d } = &path.shape else {
        panic!("a path: {:?}", path.shape)
    };
    assert!(d.contains('C') && d.ends_with('Z'), "{d}");
    assert!(path.style.fill.is_some());
}

/// Double-clicking a path edits its points: a dragged anchor is one
/// `edit-marks` of its shape.
#[test]
fn a_paths_points_are_dragged_one_edit_at_a_time() {
    let mut desk = Desk::open();
    desk.click(760.0, 600.0);
    desk.click(760.0, 600.0);
    desk.drag((620.0, 700.0), (580.0, 740.0));
    let Shape::Path { d } = desk.mark("leaf").shape else {
        panic!("still a path")
    };
    // The anchor carries its handles: the closing curve's last handle too.
    assert_eq!(
        d,
        "M580,740 C600,600 760,470 900,450 C880,600 740,730 580,740 Z"
    );
    assert!(desk.app.undo());
    let Shape::Path { d } = desk.mark("leaf").shape else {
        panic!("a path")
    };
    assert!(d.starts_with("M 620 700"), "{d}");
}

/// Shapes square up and lines straighten with Shift.
#[test]
fn shift_squares_shapes_and_straightens_lines() {
    let mut desk = Desk::open();
    desk.key("r");
    desk.hold(&["shift"]);
    desk.drag((480.0, 300.0), (580.0, 360.0));
    desk.hold(&[]);
    let made = desk.art().marks.last().cloned().unwrap();
    let (_, _, width, height) = rect_of(&made);
    assert!(
        near(width, height, 1e-6) && width > 90.0,
        "{width} {height}"
    );

    desk.key("l");
    desk.hold(&["shift"]);
    desk.drag((480.0, 420.0), (600.0, 431.0));
    desk.hold(&[]);
    let made = desk.art().marks.last().cloned().unwrap();
    let Shape::Line { points } = &made.shape else {
        panic!("a line: {:?}", made.shape)
    };
    assert!(near(points[0][1], points[1][1], 1e-6), "{points:?}");
}

/// The text tool types in place; a double click on a text retypes it.
#[test]
fn text_is_typed_in_place() {
    let mut desk = Desk::open();
    let before = desk.art().marks.len();
    desk.key("t");
    desk.click(500.0, 420.0);
    desk.app.harness().type_text("Hello");
    desk.shot("text-typing");
    desk.key("Escape");
    let art = desk.art();
    assert_eq!(art.marks.len(), before + 1);
    let made = art.marks.last().unwrap().clone();
    let Shape::Text { text, .. } = &made.shape else {
        panic!("a text")
    };
    assert_eq!(text, "Hello");

    desk.key("v");
    let area = made.shape.text_box().unwrap();
    let centre = area.center();
    desk.click(centre.x, centre.y);
    desk.click(centre.x, centre.y);
    desk.app.harness().type_text(" you");
    desk.key("Escape");
    let Shape::Text { text, .. } = desk.mark(&made.id).shape else {
        panic!("a text")
    };
    assert_eq!(text, "Hello you");
}

/// The eyedropper picks a mark's colour for the next mark.
#[test]
fn the_eyedropper_picks_up_a_colour() {
    let mut desk = Desk::open();
    desk.key("i");
    desk.click(760.0, 600.0);
    desk.key("r");
    desk.drag((480.0, 300.0), (580.0, 360.0));
    let made = desk.art().marks.last().cloned().unwrap();
    assert_eq!(made.style.fill.as_deref(), Some("#4f7a3a"));
}

/// A brush stroke is one `paint`, its pressure following the hand.
#[test]
fn a_brush_stroke_is_one_paint() {
    let mut desk = Desk::open();
    let before = desk.art().marks.len();
    desk.key("b");
    desk.hold_drag(
        (100.0, 450.0),
        &[
            (150.0, 470.0),
            (220.0, 480.0),
            (300.0, 470.0),
            (380.0, 440.0),
            (450.0, 420.0),
        ],
    );
    desk.shot("brush-painting");
    desk.button(ElementState::Released);
    let art = desk.art();
    assert_eq!(art.marks.len(), before + 1);
    let Shape::Stroke { points, .. } = &art.marks.last().unwrap().shape else {
        panic!("a stroke")
    };
    assert!(points.len() >= 4, "{points:?}");
    assert!(points[0][2] < 0.5, "it lands lightly");
}

/// Zooming in makes a drag move things less far; Shift+2 and Shift+1
/// zoom to the selection and back to the whole artwork.
#[test]
fn zoom_keys_change_how_far_a_drag_goes() {
    let mut desk = Desk::open();
    desk.hold(&["ctrl"]);
    desk.key("a");
    desk.hold(&[]);
    desk.key("+");
    desk.key("+");
    let zoom = desk.zoom * 1.25 * 1.25;
    let centre = desk.canvas.center();
    desk.hold(&["ctrl"]);
    desk.app
        .harness()
        .drag_at((centre.x, centre.y), (centre.x + 50.0, centre.y));
    desk.hold(&[]);
    let (x, ..) = rect_of(&desk.mark("card"));
    assert!(near(x, 120.0 + 50.0 / zoom, 0.05), "{x}");
    desk.hold(&["shift"]);
    desk.key("@");
    desk.shot("zoomed-to-selection");
    desk.key("!");
    desk.hold(&[]);
}

/// Screenshots of the canvas mid-gesture, for a person to judge, in
/// `target/look/canvas-interact`:
/// `cargo test -p canvas --test interact -- --ignored`.
fn shots() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/look/canvas-interact")
}

#[test]
#[ignore = "writes screenshots to look at"]
fn screenshots_of_gestures_in_progress() {
    for scenario in ["selection", "turned", "points", "close-up", "landscape"] {
        let mut app = TestApp::<App>::capture(Some(scenario)).unwrap();
        let mut desk = Desk {
            canvas: app.harness().targets().get("canvas").copied().unwrap(),
            app,
            origin: (0.0, 0.0),
            zoom: 1.0,
            shots: Some(shots()),
        };
        desk.shot(&format!("scenario-{scenario}"));
    }
    let mut desk = Desk::open();
    desk.shots = Some(shots());
    desk.hold_drag((270.0, 240.0), &[(200.0, 300.0), (152.0, 300.0)]);
    desk.shot("moving-with-guides");
    desk.button(ElementState::Released);
    desk.hold_drag((420.0, 280.0), &[(500.0, 300.0), (980.0, 860.0)]);
    desk.shot("marquee");
    desk.button(ElementState::Released);
    desk.shot("marquee-picked");
    desk.key("Escape");
    desk.click(300.0, 620.0);
    // The turned card's right edge handle: (420, 620) turned -12 degrees
    // about (300, 620).
    let turn = (-12.0_f64).to_radians();
    let right = (300.0 + 120.0 * turn.cos(), 620.0 + 120.0 * turn.sin());
    desk.hold_drag(
        right,
        &[(right.0 + 30.0, right.1), (right.0 + 80.0, right.1 - 10.0)],
    );
    desk.shot("stretching-turned");
    desk.button(ElementState::Released);
    desk.key("p");
    desk.click(500.0, 400.0);
    desk.hold_drag((700.0, 420.0), &[(740.0, 460.0), (780.0, 500.0)]);
    desk.shot("pen-pulling-a-curve");
    desk.button(ElementState::Released);
    desk.pointer(600.0, 600.0);
    desk.shot("pen-rubber-band");
    desk.key("Escape");
    desk.key("t");
    desk.click(440.0, 120.0);
    desk.app.harness().type_text("Typed in place");
    desk.key("Enter");
    desk.app.harness().type_text("on two lines");
    desk.key("ArrowLeft");
    desk.key("ArrowLeft");
    desk.shot("text-typing");
    desk.key("Escape");
    desk.key("v");
    desk.click(760.0, 600.0);
    desk.hold(&["shift"]);
    desk.key("@");
    desk.hold(&[]);
    desk.pointer(700.0, 650.0);
    desk.shot("zoomed-to-selection");

    // A stroke on the oil layer, shown as the oil will paint it before the
    // brush lifts.
    let mut app = TestApp::<App>::capture(Some("landscape")).unwrap();
    app.harness()
        .perform_named("canvas-layer", &ui::a11y::Action::SetValue("land".into()))
        .unwrap();
    let canvas = app.harness().targets().get("canvas").copied().unwrap();
    let art = app.state();
    let zoom = ((canvas.width() - 48.0) / art.width).min((canvas.height() - 48.0) / art.height);
    let mut desk = Desk {
        origin: (canvas.center().x - art.width * zoom / 2.0, canvas.y0 + 24.0),
        zoom,
        canvas,
        app,
        shots: Some(shots()),
    };
    desk.app
        .harness()
        .perform_named("canvas", &ui::a11y::Action::Focus)
        .unwrap();
    desk.key("b");
    let started = std::time::Instant::now();
    desk.hold_drag(
        (300.0, 300.0),
        &[
            (380.0, 330.0),
            (460.0, 350.0),
            (560.0, 340.0),
            (660.0, 300.0),
            (760.0, 280.0),
        ],
    );
    eprintln!("six wet previews took {:?}", started.elapsed());
    desk.shot("wet-oil-preview");
    desk.button(ElementState::Released);
    desk.shot("wet-oil-painted");
}
