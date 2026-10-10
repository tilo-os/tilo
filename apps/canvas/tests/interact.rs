//! Working on the canvas with the pointer and keys, as a person does: every
//! gesture lands as one of the artwork's actions, one step of history.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::too_many_lines,
    reason = "test assertions over exact, hand-written fixtures"
)]

mod support;

use canvas::model::{Mark, Shape};
use canvas::{App, Artwork};
use ui::prelude::TestApp;
use ui::winit::event::{ElementState, MouseButton, WindowEvent};

#[test]
fn fit_from_a_focused_brush_field_restores_pointer_artwork_coordinates() {
    let mut desk = Desk::open();
    desk.key("b");
    desk.key("Ctrl+1");
    desk.app
        .harness()
        .perform_named("canvas-brush-colour", &ui::a11y::Action::Focus)
        .unwrap();
    desk.key("Ctrl+0");
    let before = desk.art().marks.len();
    desk.drag((100.0, 100.0), (250.0, 100.0));
    let art = desk.art();
    assert_eq!(art.marks.len(), before + 1);
    let Shape::Stroke { points, .. } = &art.marks.last().unwrap().shape else {
        panic!("the pointer should lay a brush stroke");
    };
    let first = points.first().unwrap();
    let last = points.last().unwrap();
    assert!((first[0] - 100.0).abs() < 1.0, "{points:?}");
    assert!((first[1] - 100.0).abs() < 1.0, "{points:?}");
    assert!((last[0] - 250.0).abs() < 1.0, "{points:?}");
    assert!((last[1] - 100.0).abs() < 1.0, "{points:?}");
}

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

#[test]
fn an_artwork_swatch_loads_the_brush_and_the_pointer_uses_that_paint() {
    for colour in ["#010203", "#01020380"] {
        let mut desk = Desk::open();
        desk.app.call("draw", serde_json::json!({"marks":[{
            "id":"custom-paint", "shape":{"kind":"rect","x":400,"y":300,"width":20,"height":20},
            "fill":colour
        }]})).unwrap();
        desk.key("b");
        let palette = desk.app.harness().targets()["canvas-artwork-colours"];
        // First document swatch; pointer through the shared picker.
        desk.app
            .harness()
            .click_at(palette.x0 + 14.0, palette.y0 + 14.0);
        let before = desk.art().marks.len();
        desk.drag((100.0, 100.0), (200.0, 100.0));
        assert_eq!(desk.art().marks.len(), before + 1);
        assert_eq!(
            desk.art().marks.last().unwrap().style.stroke.as_deref(),
            Some(colour)
        );
        assert!(desk.app.undo(), "one gesture is one framework undo step");
        assert_eq!(desk.art().marks.len(), before);
        assert!(desk.app.redo());
        assert_eq!(
            desk.art().marks.last().unwrap().style.stroke.as_deref(),
            Some(colour)
        );
    }
}

impl Desk {
    fn open() -> Self {
        // A double click is two clicks within the session's interval; a
        // busy test machine takes its time between them.
        ui::gesture::set_multi_click_interval(std::time::Duration::from_secs(10));
        let mut app = TestApp::<App>::new().expect("the app opens");
        for action in canvas::sample::shapes() {
            app.invoke(action).expect("the sample draws");
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
        if let Some((modifier, key)) = key.split_once('+')
            && matches!(
                modifier.to_ascii_lowercase().as_str(),
                "ctrl" | "shift" | "alt"
            )
        {
            self.hold(&[modifier]);
            self.app.harness().key(key);
            self.hold(&[]);
        } else {
            self.app.harness().key(key);
        }
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
            .render_png(&dir.join(format!("{name}.png")));
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

/// The eraser takes paint off as a mark of its own; switched to whole
/// strokes, it takes every stroke it crosses in one step.
#[test]
fn the_eraser_rubs_out_paint_or_whole_strokes() {
    let mut desk = Desk::open();
    desk.key("b");
    desk.drag((100.0, 450.0), (450.0, 450.0));
    let painted = desk.art().marks.last().unwrap().id.clone();
    desk.key("x");
    desk.drag((275.0, 400.0), (275.0, 500.0));
    let rub = desk.art().marks.last().cloned().unwrap();
    assert_eq!(rub.style.brush.as_deref(), Some("eraser"));
    assert!(rub.style.stroke.is_none(), "an eraser has no colour");
    desk.shot("eraser-paint");

    let modes = desk.app.harness().targets()["canvas-erasing"];
    desk.app
        .harness()
        .click_at(modes.x1 - modes.width() / 4.0, modes.center().y);
    let before = desk.art().marks.len();
    desk.drag((200.0, 400.0), (210.0, 500.0));
    let art = desk.art();
    assert!(art.mark(&painted).is_none(), "the crossed stroke is gone");
    assert_eq!(art.marks.len(), before - 1);
    assert!(desk.app.undo());
    assert!(
        desk.art().mark(&painted).is_some(),
        "one undo brings it back"
    );
}

/// An eraser has no ink, but it is a mark like any other: the selection
/// tool picks it where it rubbed (its outline shows while that tool is in
/// hand), and Delete takes it away, the paint under it back.
#[test]
fn an_eraser_is_picked_and_removed_like_a_mark() {
    let mut desk = Desk::open();
    desk.key("b");
    desk.drag((100.0, 450.0), (450.0, 450.0));
    let painted = desk.art().marks.last().unwrap().id.clone();
    desk.key("x");
    desk.drag((275.0, 400.0), (275.0, 500.0));
    let rub = desk.art().marks.last().unwrap().id.clone();
    desk.key("v");
    desk.pointer(275.0, 470.0);
    desk.shot("eraser-outlined");
    desk.click(275.0, 470.0);
    desk.shot("eraser-picked");
    desk.key("Delete");
    let art = desk.art();
    assert!(art.mark(&rub).is_none(), "the picked eraser is removed");
    assert!(art.mark(&painted).is_some(), "the paint it rubbed stays");
}

/// The eyedropper takes the colour the canvas shows: where an eraser took
/// the paint off, the ground beneath, not the erased stroke's ink.
#[test]
fn the_eyedropper_sees_what_an_eraser_left() {
    let mut desk = Desk::open();
    desk.key("b");
    desk.drag((100.0, 450.0), (450.0, 450.0));
    let ink = desk.art().marks.last().unwrap().style.stroke.clone();
    desk.key("x");
    desk.drag((275.0, 400.0), (275.0, 500.0));
    let picked = |desk: &mut Desk, at: (f64, f64), corner: f64| {
        desk.key("i");
        desk.click(at.0, at.1);
        desk.key("r");
        desk.drag((corner, 40.0), (corner + 40.0, 70.0));
        desk.art().marks.last().unwrap().style.fill.clone()
    };
    let beside = picked(&mut desk, (150.0, 450.0), 40.0);
    let erased = picked(&mut desk, (275.0, 450.0), 120.0);
    assert_eq!(beside, ink, "beside the eraser the ink shows");
    assert_ne!(erased, ink, "under the eraser it does not");
    // The ground, give or take its paper's tooth.
    let ground = desk.art().ground.colour;
    let channels = |hex: &str| -> Vec<i32> {
        (1..7)
            .step_by(2)
            .map(|at| i32::from_str_radix(hex.get(at..at + 2).unwrap(), 16).unwrap())
            .collect()
    };
    let erased = channels(erased.as_deref().unwrap());
    assert!(
        erased
            .iter()
            .zip(channels(&ground))
            .all(|(a, b)| (a - b).abs() <= 8),
        "the ground shows: {erased:?} against {ground}"
    );
}

/// A pen paints with its own pressure, whatever the hand's speed; its
/// eraser end erases.
#[test]
fn a_pen_paints_with_its_pressure_and_erases_with_its_other_end() {
    let mut desk = Desk::open();
    desk.key("b");
    let pen = |desk: &mut Desk, pressure: f64, eraser: bool| {
        desk.app.harness().send(&WindowEvent::PenSample {
            device_id: ui::winit::event::DeviceId::dummy(),
            pressure,
            tilt: (0.0, 0.0),
            eraser,
        });
    };
    pen(&mut desk, 0.1, false);
    desk.pointer(100.0, 450.0);
    pen(&mut desk, 0.1, false);
    desk.button(ElementState::Pressed);
    for (step, x) in [150.0, 200.0, 250.0, 300.0, 350.0, 400.0]
        .into_iter()
        .enumerate()
    {
        pen(
            &mut desk,
            0.25 + 0.14 * f64::from(u32::try_from(step).unwrap()),
            false,
        );
        desk.pointer(x, 450.0);
    }
    pen(&mut desk, 0.95, false);
    desk.button(ElementState::Released);
    let stroke = desk.art().marks.last().cloned().unwrap();
    let Shape::Stroke { points, .. } = &stroke.shape else {
        panic!("a stroke")
    };
    assert!(
        points[0][2] < 0.15,
        "it lands as lightly as the pen: {points:?}"
    );
    assert!(
        points.last().unwrap()[2] > 0.8,
        "it lifts as hard as the pen pressed: {points:?}"
    );

    for (pressure, at) in [(0.8, (200.0, 400.0)), (0.8, (200.0, 500.0))] {
        pen(&mut desk, pressure, true);
        desk.pointer(at.0, at.1);
        if at.1 < 450.0 {
            pen(&mut desk, pressure, true);
            desk.button(ElementState::Pressed);
        }
    }
    pen(&mut desk, 0.8, true);
    desk.button(ElementState::Released);
    let rub = desk.art().marks.last().cloned().unwrap();
    assert_eq!(rub.style.brush.as_deref(), Some("eraser"), "{rub:?}");
}

/// With a stroke selected, the brush landing on its end carries it on; a
/// double click edits its path, the stretch near a dragged point bending.
#[test]
fn a_selected_stroke_is_carried_on_and_bent() {
    let mut desk = Desk::open();
    desk.key("b");
    desk.drag((100.0, 450.0), (300.0, 450.0));
    let id = desk.art().marks.last().unwrap().id.clone();
    let count = desk.art().marks.len();
    desk.key("v");
    desk.click(200.0, 450.0);
    desk.key("b");
    desk.drag((300.0, 450.0), (420.0, 420.0));
    assert_eq!(
        desk.art().marks.len(),
        count,
        "carried on, not a new stroke"
    );
    let Shape::Stroke { points, .. } = desk.mark(&id).shape else {
        panic!("a stroke")
    };
    let end = points.last().unwrap();
    assert!(
        near(end[0], 420.0, 0.5) && near(end[1], 420.0, 0.5),
        "{end:?}"
    );

    desk.key("v");
    desk.click(200.0, 450.0);
    desk.click(200.0, 450.0);
    desk.drag((420.0, 420.0), (420.0, 380.0));
    let Shape::Stroke { points, .. } = desk.mark(&id).shape else {
        panic!("a stroke")
    };
    assert!(near(points.last().unwrap()[1], 380.0, 0.5), "{points:?}");
    assert!(
        near(points[0][1], 450.0, 0.5),
        "the far end stays: {points:?}"
    );
}

/// Zooming in makes a drag move things less far; Shift+2 and Shift+1
/// zoom to the selection and back to the whole artwork.
#[test]
fn zoom_keys_change_how_far_a_drag_goes() {
    let mut desk = Desk::open();
    desk.hold(&["ctrl"]);
    desk.key("a");
    desk.hold(&[]);
    desk.key("=");
    desk.key("=");
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

/// A mask rides in its layer's row: its picture beside the layer's, each
/// choosing what the brush paints, and the strip over the canvas saying
/// while it is the mask.
#[test]
fn a_mask_is_painted_from_its_picture_in_the_layers_row() {
    let mut desk = Desk::open();
    let layer = desk.art().top_layer().unwrap().id.clone();
    desk.app
        .invoke(canvas::Artwork::SET_LAYER_MASK.with((layer.clone(), "reveal".into())))
        .expect("a mask is added");
    let mask = desk.art().mask_of(&layer).unwrap().id.clone();
    let targets = desk.app.harness().targets();
    assert!(
        !targets.contains_key(&format!("canvas-layer-name-{mask}")),
        "the mask has no row of its own"
    );
    // The layer the next brush stroke lands on.
    let stroke = |desk: &mut Desk| {
        let before: Vec<String> = desk
            .art()
            .marks
            .iter()
            .map(|mark| mark.id.clone())
            .collect();
        desk.key("b");
        desk.drag((100.0, 450.0), (300.0, 470.0));
        let art = desk.art();
        let new = art.marks.iter().find(|mark| !before.contains(&mark.id));
        new.expect("a stroke is laid").layer.clone()
    };
    // A mask just added is what is painted next.
    assert_eq!(stroke(&mut desk), mask);
    assert!(
        desk.app
            .harness()
            .targets()
            .contains_key("canvas-paint-layer"),
        "the strip offers the way back to the layer"
    );
    desk.shot("mask-painting");
    desk.app
        .harness()
        .click_named(&format!("canvas-paint-{layer}"))
        .expect("the layer's picture");
    assert_eq!(stroke(&mut desk), layer);
    desk.app
        .harness()
        .click_named(&format!("canvas-paint-{mask}"))
        .expect("the mask's picture");
    assert_eq!(stroke(&mut desk), mask);
    desk.app
        .harness()
        .click_named("canvas-paint-layer")
        .expect("back to the layer from the strip");
    assert_eq!(stroke(&mut desk), layer);
}

/// Picked marks line up from the strip over the canvas, each step one
/// edit; a single picked mark lines up on the artwork.
#[test]
fn the_strip_lines_picked_marks_up() {
    let mut desk = Desk::open();
    desk.click(270.0, 240.0);
    desk.hold(&["shift"]);
    desk.click(760.0, 600.0);
    desk.hold(&[]);
    desk.app
        .harness()
        .click_named("canvas-align-top")
        .expect("the strip offers lining up their tops");
    let art = desk.art();
    let top = |id: &str| art.visible_bounds(art.mark(id).unwrap()).y0;
    assert!(
        near(top("card"), top("leaf"), 1e-6),
        "{} {}",
        top("card"),
        top("leaf")
    );
    desk.shot("align-strip");
    // The strip's button has the focus now; the canvas's keys still act
    // on what is picked.
    let before = desk.art().marks.len();
    desk.key("Delete");
    assert_eq!(
        desk.art().marks.len(),
        before - 2,
        "Delete removes the picked marks"
    );
    assert!(desk.app.undo());
    assert!(desk.app.undo(), "lining up is one step");

    desk.key("Escape");
    desk.click(270.0, 240.0);
    desk.app
        .harness()
        .click_named("canvas-align-center")
        .expect("one picked mark lines up on the artwork");
    let art = desk.art();
    let card = art.visible_bounds(art.mark("card").unwrap());
    assert!(
        near(card.center().x, art.width / 2.0, 1e-6),
        "{} {}",
        card.center().x,
        art.width / 2.0
    );
}

/// A click a few pixels off a thin line still picks it, however far out
/// the artwork is zoomed.
#[test]
fn a_click_just_off_a_thin_line_picks_it() {
    let mut desk = Desk::open();
    assert!(desk.zoom < 0.8, "the test needs a far zoom: {}", desk.zoom);
    desk.key("l");
    desk.drag((100.0, 870.0), (900.0, 870.0));
    let line = desk.art().marks.last().cloned().expect("a line is drawn");
    assert!(matches!(line.shape, Shape::Line { .. }), "{:?}", line.shape);
    desk.key("Escape");
    desk.key("v");
    desk.click(40.0, 40.0);
    // Three and a half screen pixels past the line's edge.
    let off = line.style.stroke_width / 2.0 + 3.5 / desk.zoom;
    desk.click(500.0, 870.0 - off);
    desk.key("Delete");
    assert!(
        desk.art().mark(&line.id).is_none(),
        "the click picked the line"
    );
}

/// `[` and `]` make the brush smaller and larger.
#[test]
fn brackets_resize_the_brush() {
    let mut desk = Desk::open();
    desk.key("b");
    let laid = |desk: &mut Desk, y: f64| {
        desk.drag((100.0, y), (300.0, y + 10.0));
        match desk.art().marks.last().map(|mark| mark.shape.clone()) {
            Some(Shape::Stroke { size, .. }) => size,
            other => panic!("a stroke is laid, not {other:?}"),
        }
    };
    let before = laid(&mut desk, 850.0);
    for _ in 0..6 {
        desk.key("]");
    }
    desk.pointer(500.0, 600.0);
    desk.shot("brush-footprint");
    desk.key("]");
    desk.key("]");
    let larger = laid(&mut desk, 865.0);
    desk.key("[");
    let smaller = laid(&mut desk, 880.0);
    assert!(
        larger > before && smaller < larger,
        "{before} {larger} {smaller}"
    );
}

/// The strip grows, shrinks and softens the selection by a step seen at
/// the zoom it is shown at, one edit each.
#[test]
fn the_strip_refines_the_selection() {
    let mut desk = Desk::open();
    desk.key("m");
    desk.drag((100.0, 400.0), (300.0, 500.0));
    let width = |desk: &Desk| {
        desk.art()
            .selection
            .as_ref()
            .expect("an area is selected")
            .region()
            .bounds()
            .expect("it has an extent")
            .width()
    };
    let before = width(&desk);
    desk.app
        .harness()
        .click_named("canvas-area-grow")
        .expect("the strip offers growing it");
    let grown = width(&desk);
    // Four screen pixels each side.
    assert!(
        near(grown - before, 8.0 / desk.zoom, 0.5),
        "{before} {grown}"
    );
    desk.app
        .harness()
        .click_named("canvas-area-feather")
        .expect("the strip offers softening it");
    let feather = desk.art().selection.as_ref().unwrap().feather;
    assert!(near(feather, 8.0 / desk.zoom, 1e-6), "{feather}");
}

#[test]
fn area_tools_show_subject_controls_while_a_mark_stays_selected() {
    let mut desk = Desk::open();
    desk.click(270.0, 240.0);
    let selected = desk
        .app
        .call("inspect", serde_json::json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(selected["selected"], serde_json::json!(["card"]));
    for key in ["m", "o", "q", "w"] {
        desk.key(key);
        assert!(
            desk.app
                .harness()
                .targets()
                .contains_key("canvas-select-subject"),
            "{key} must expose its controls without an extra deselect"
        );
        assert!(
            desk.app.harness().resources().iter().any(|resource| {
                resource.kind == "canvas.artwork"
                    && resource
                        .actions
                        .iter()
                        .any(|action| action.id == "find-area")
            }),
            "the same control must be reachable through actions"
        );
        let current = desk
            .app
            .call("inspect", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(current["selected"], selected["selected"]);
    }
    desk.key("v");
    assert!(
        !desk
            .app
            .harness()
            .targets()
            .contains_key("canvas-select-subject")
    );
    let current = desk
        .app
        .call("inspect", serde_json::json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(current["selected"], selected["selected"]);
}

#[test]
fn object_tree_selects_an_occluded_object_for_an_exact_edit() {
    let mut desk = Desk::open();
    desk.app.call("draw",serde_json::json!({"marks":[
        {"id":"covered","name":"Hidden rectangle","shape":{"kind":"rect","x":10,"y":10,"width":100,"height":100},"fill":"#010203"},
        {"id":"cover","shape":{"kind":"rect","x":0,"y":0,"width":200,"height":200},"fill":"#ffffff"}
    ]})).unwrap();
    desk.app
        .harness()
        .perform_semantic_named(
            "canvas-dock",
            Some("canvas-dock:segment:1"),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    let snapshot = desk.app.harness().inspection();
    let target = support::named_bounds(&snapshot.root, "Hidden rectangle")
        .expect("the covered object is listed");
    desk.app
        .harness()
        .click_at(target.center().x, target.center().y);
    desk.key("ArrowRight");
    let Shape::Rect { x, .. } = desk.mark("covered").shape else {
        panic!("rectangle");
    };
    assert_eq!(
        x, 11.0,
        "tree selection reaches the production nudge action"
    );
    let Shape::Rect { x, .. } = desk.mark("cover").shape else {
        panic!("rectangle");
    };
    assert_eq!(x, 0.0, "the covering object is untouched");
    assert!(desk.app.undo());
    let Shape::Rect { x, .. } = desk.mark("covered").shape else {
        panic!("rectangle");
    };
    assert_eq!(x, 10.0);
}

#[test]
fn pixel_pinch_zoom_keeps_the_cell_under_the_pointer() {
    let mut desk = Desk::open();
    desk.key("b");
    desk.app
        .harness()
        .perform_semantic_named(
            "canvas-workspace",
            Some("canvas-workspace:segment:3"),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    desk.key("Ctrl+1");
    desk.key("=");
    let focal = (desk.canvas.center().x + 60.0, desk.canvas.center().y + 45.0);
    desk.app.harness().send(&WindowEvent::CursorMoved {
        device_id: ui::winit::event::DeviceId::dummy(),
        position: ui::winit::dpi::PhysicalPosition::new(focal.0, focal.1),
    });
    // Both axes overflow at 2x: no centering or scroll-edge clamp can move the anchor.
    // Paint, zoom, and paint again at the same screen point.
    desk.app.harness().click_at(focal.0, focal.1);
    let first = desk.art().marks.last().unwrap().shape.clone();
    desk.app.harness().send(&WindowEvent::PinchGesture {
        device_id: ui::winit::event::DeviceId::dummy(),
        delta: 0.25,
        phase: ui::winit::event::TouchPhase::Moved,
    });
    desk.app.harness().click_at(focal.0, focal.1);
    let after = desk.art().marks.last().unwrap().shape.clone();
    assert_eq!(
        first, after,
        "the same cell stays under the zoom focal point"
    );
}

#[test]
fn a_large_pixel_sheet_fits_below_actual_size_and_paints_in_artwork_cells() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call(
        "new-artwork",
        serde_json::json!({"width":4096,"height":4096}),
    )
    .unwrap();
    app.harness()
        .perform_semantic_named(
            "canvas-workspace",
            Some("canvas-workspace:segment:3"),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    app.harness().key("Ctrl+0");
    let canvas = app.harness().targets()["canvas"];
    let center = canvas.center();
    for x in [center.x - 30.0, center.x + 30.0] {
        app.harness().click_at(x, center.y);
    }
    let art = app.state();
    assert_eq!(art.marks.len(), 2);
    let coordinate = |mark: &Mark| {
        let Shape::Stroke { points, .. } = &mark.shape else {
            panic!("pixel stroke");
        };
        points[0][0]
    };
    let cells = coordinate(&art.marks[1]) - coordinate(&art.marks[0]);
    assert!(
        cells >= 120.0,
        "Fit must reduce the sheet below 100%: {cells} cells across 60 screen pixels"
    );
    assert!(
        (cells / 60.0 - (cells / 60.0).round()).abs() < 1e-6,
        "pixel reductions are reciprocal whole-cell steps"
    );
}

#[test]
fn opening_a_differently_sized_artwork_fits_after_manual_pixel_zoom() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call("new-artwork", serde_json::json!({"width":128,"height":128}))
        .unwrap();
    app.harness()
        .perform_semantic_named(
            "canvas-workspace",
            Some("canvas-workspace:segment:3"),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    app.harness().key("Ctrl+1");
    app.harness().key("+");
    app.harness().key("+");
    app.call(
        "new-artwork",
        serde_json::json!({"width":4096,"height":4096}),
    )
    .unwrap();
    let saved = app
        .call("save-artwork", serde_json::json!({}))
        .unwrap()
        .unwrap();
    app.call("new-artwork", serde_json::json!({"width":128,"height":128}))
        .unwrap();
    app.harness().key("Ctrl+1");
    app.harness().key("+");
    app.call("open-artwork", serde_json::json!({"file":saved["content"]}))
        .unwrap();
    let canvas = app.harness().targets()["canvas"];
    app.harness()
        .click_at(canvas.center().x + 30.0, canvas.center().y);
    let art = app.state();
    let Shape::Stroke { points, .. } = &art.marks[0].shape else {
        panic!("pixel stroke");
    };
    assert!(
        points[0][0] > 2100.0,
        "new artwork is fitted, not painted through the old 2x zoom: {:?}",
        points[0]
    );
}

#[test]
fn pixel_pinch_follows_small_deltas_instead_of_jumping_whole_zoom_levels() {
    small_pixel_zoom_deltas(false);
}

#[test]
fn pixel_trackpad_wheel_follows_small_deltas_instead_of_jumping_whole_zoom_levels() {
    small_pixel_zoom_deltas(true);
}

fn small_pixel_zoom_deltas(wheel: bool) {
    let mut desk = Desk::open();
    desk.app
        .harness()
        .perform_semantic_named(
            "canvas-workspace",
            Some("canvas-workspace:segment:3"),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    desk.key("Ctrl+1");
    desk.key("=");
    let canvas = *desk
        .app
        .harness()
        .targets()
        .get("canvas")
        .expect("the canvas is visible");
    let focal = canvas.center();
    desk.app.harness().click_at(focal.x, focal.y);
    let anchor = desk.art().marks.last().unwrap().shape.clone();
    if wheel {
        desk.hold(&["ctrl"]);
    }
    for _ in 0..20 {
        let event = if wheel {
            WindowEvent::MouseWheel {
                device_id: ui::winit::event::DeviceId::dummy(),
                delta: ui::winit::event::MouseScrollDelta::PixelDelta(
                    ui::winit::dpi::PhysicalPosition::new(0.0, 2.4),
                ),
                phase: ui::winit::event::TouchPhase::Moved,
            }
        } else {
            WindowEvent::PinchGesture {
                device_id: ui::winit::event::DeviceId::dummy(),
                delta: 0.01,
                phase: ui::winit::event::TouchPhase::Moved,
            }
        };
        desk.app.harness().send(&event);
    }
    desk.hold(&[]);
    desk.app.harness().click_at(focal.x, focal.y);
    assert_eq!(
        desk.art().marks.last().unwrap().shape,
        anchor,
        "small deltas preserve the focal cell"
    );
    desk.app.harness().click_at(focal.x + 100.0, focal.y);
    let art = desk.art();
    let x = |shape: &Shape| match shape {
        Shape::Stroke { points, .. } => points.first().and_then(|point| point.first()).copied(),
        _ => None,
    };
    let span = x(&art.marks.last().unwrap().shape).expect("a pixel stroke")
        - x(&anchor).expect("a pixel anchor stroke");
    assert!(
        (40.0..=43.0).contains(&span),
        "twenty 1% inputs should enlarge 2x to about 2.44x, not jump through twenty integer scales: {span}"
    );
}

#[test]
fn mouse_wheel_zoom_requests_frames_until_the_motion_settles() {
    // This measures zoom motion, not startup image jobs on a shared worker pool.
    // Keep the real animation clock and the existing two-second motion bound.
    let pumped = ui::background::Pumped::new();
    let _entered = pumped.background().enter();
    let mut desk = Desk::open();
    desk.app
        .call("set-artwork", serde_json::json!({"texture":"smooth"}))
        .unwrap();
    pumped.run_pending();
    desk.key("Ctrl+1");
    desk.key("=");
    let canvas = desk.app.harness().targets()["canvas"];
    let focal = canvas.center();
    desk.app.harness().send(&WindowEvent::CursorMoved {
        device_id: ui::winit::event::DeviceId::dummy(),
        position: ui::winit::dpi::PhysicalPosition::new(focal.x, focal.y),
    });
    desk.hold(&["ctrl"]);
    // Observe scheduling at the actual input boundary. `scroll` also paints,
    // which can legitimately outlast the 130 ms interpolation on a busy host.
    assert!(
        desk.app
            .harness()
            .dispatch_to_root(&WindowEvent::MouseWheel {
                device_id: ui::winit::event::DeviceId::dummy(),
                delta: ui::winit::event::MouseScrollDelta::LineDelta(0.0, 1.0),
                phase: ui::winit::event::TouchPhase::Moved,
            })
    );
    assert!(
        desk.app.harness().frame_schedule().needs_animation_frame,
        "one wheel notch schedules intermediate frames without further input"
    );
    desk.hold(&[]);
    assert!(
        desk.app
            .harness()
            .settle_for_capture(tilo_test_env::hang_bound()),
        "the shared motion driver settles"
    );
    assert!(
        !desk.app.harness().frame_schedule().needs_animation_frame,
        "zoom stops scheduling frames after settling"
    );
}

#[test]
fn enabling_reduced_motion_finishes_an_in_flight_wheel_zoom() {
    fn has_label(node: &ui::headless::InspectionNode, name: &str) -> bool {
        node.semantics
            .as_ref()
            .is_some_and(|semantic| semantic.name.as_deref() == Some(name))
            || node.children.iter().any(|child| has_label(child, name))
    }
    // Drive icon/background completions deterministically; this exercises
    // motion policy, not the shared runner's worker scheduling latency.
    let pumped = ui::background::Pumped::new();
    let _entered = pumped.background().enter();
    let mut desk = Desk::open();
    // A smooth ground keeps scheduling demand focused on zoom policy.
    desk.app
        .call("set-artwork", serde_json::json!({"texture":"smooth"}))
        .unwrap();
    desk.app.harness().resize((1280, 900)).unwrap();
    pumped.run_pending();
    desk.key("Ctrl+1");
    desk.key("=");
    let canvas = desk.app.harness().targets()["canvas"];
    desk.app.harness().send(&WindowEvent::CursorMoved {
        device_id: ui::winit::event::DeviceId::dummy(),
        position: ui::winit::dpi::PhysicalPosition::new(canvas.center().x, canvas.center().y),
    });
    desk.hold(&["ctrl"]);
    // Inspect the actual wheel handler before the host's next layout/draw.
    // `scroll` also paints that frame, whose cost may exceed the ordinary
    // 130 ms interpolation on a busy runner. The pumped executor controls
    // background work, not the toolkit's real monotonic animation clock.
    assert!(
        desk.app
            .harness()
            .dispatch_to_root(&WindowEvent::MouseWheel {
                device_id: ui::winit::event::DeviceId::dummy(),
                delta: ui::winit::event::MouseScrollDelta::LineDelta(0.0, 1.0),
                phase: ui::winit::event::TouchPhase::Moved,
            })
    );
    assert!(desk.app.harness().frame_schedule().needs_animation_frame);
    let mut theme = ui::Theme::dark();
    theme.reduce_motion = true;
    desk.app.harness().set_theme(theme);
    desk.hold(&[]);
    assert!(
        has_label(&desk.app.harness().inspection().root, "154%"),
        "reduced motion immediately reaches the wheel's target zoom"
    );
    pumped.run_pending();
    assert!(
        desk.app
            .harness()
            .settle_for_capture(tilo_test_env::hang_bound()),
        "the window settles: {:?}; image loads in flight: {}",
        desk.app.harness().frame_requesters(),
        ui::image_store::loads_in_flight()
    );
    desk.hold(&["ctrl"]);
    desk.app.harness().scroll(0.0, 1.0);
    desk.hold(&[]);
    assert!(
        has_label(&desk.app.harness().inspection().root, "190%"),
        "subsequent wheel input immediately reaches its target zoom"
    );
    pumped.run_pending();
    assert!(
        desk.app
            .harness()
            .settle_for_capture(tilo_test_env::hang_bound())
    );
    assert!(
        !desk.app.harness().frame_schedule().needs_animation_frame,
        "reduced-motion wheel zoom completes without scheduling interpolation frames"
    );
}
