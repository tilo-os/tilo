//! A map's pins under the camera's zoom: a pin's view is laid out at its place's own size, at
//! rest, and drawn through the camera's transform, so what it shows reads at any zoom; and the
//! camera only moves and scales the pin, never lays its view out again.

use super::*;
use crate::Theme;
use crate::widget::{Button, Column, Text};

fn theme() -> Theme {
    Theme::dark()
}

/// A pin's view: a text label (a glyph run in the scene when it is drawn) and a button that
/// counts its presses.
fn view(clicks: std::rc::Rc<std::cell::Cell<u32>>) -> Element {
    let button = Button::new("press", "Press").on_click(move || {
        clicks.set(clicks.get() + 1);
    });
    Box::new(
        Column::new()
            .with_no_padding()
            .push(Box::new(Text::new("A note on the canvas")))
            .push(Box::new(button)),
    )
}

/// The translation of every glyph run the scene paints, in window logical pixels: where the
/// camera drew each line of the pin's label.
fn glyph_runs(scene: &Scene) -> Vec<(f64, f64)> {
    scene
        .encoding()
        .resources
        .glyph_runs
        .iter()
        .map(|run| {
            (
                f64::from(run.transform.translation[0]),
                f64::from(run.transform.translation[1]),
            )
        })
        .collect()
}

#[test]
fn a_pin_reads_at_a_quarter_zoom_and_its_button_presses_where_it_is_drawn() {
    let _frame = crate::motion::pin_frame();
    let clicks = std::rc::Rc::new(std::cell::Cell::new(0_u32));
    let mut map = Map::new().pin(
        "place",
        PinPlace {
            x: 40.0,
            y: 40.0,
            width: 320.0,
            height: Some(200.0),
        },
        view(clicks.clone()),
    );
    // A quarter zoom: at the place size times 0.25 a rem-sized label would wrap to nothing and
    // the bars would collapse. Laid out at the place's own size and drawn scaled, the pin reads.
    map.set_camera(MapCamera {
        origin: Point::ZERO,
        zoom: 0.25,
    });

    let mut harness =
        crate::headless::FrameHarness::for_root(crate::State::new(), map, (800, 600), theme());
    harness.relayout();
    let scene = draw_root(&mut harness);
    let runs = glyph_runs(&scene);
    assert!(
        !runs.is_empty(),
        "the pin's label is drawn at a quarter zoom"
    );
    // The label is drawn through the camera: its glyph run lands inside the pin's box on
    // screen, at a quarter of the place's size.
    let pin_screen = harness
        .root::<Map>()
        .host_of("place")
        .expect("the pin")
        .screen;
    let (x, y) = runs[0];
    assert!(
        pin_screen.inflate(1.0, 1.0).contains(Point::new(x, y)),
        "the label is drawn inside the pin at {pin_screen:?}: run at ({x}, {y})"
    );
    assert!(
        (pin_screen.width() - 320.0 * 0.25).abs() < 1.0,
        "the pin is a quarter of its place's width on screen: {pin_screen:?}"
    );

    // The button's box in the pin's own space, drawn at a quarter scale: a press where the
    // camera drew it reaches it, and the view's button is pressed.
    let button_at = crate::widget::find_test_id(harness.root::<Map>(), "press")
        .expect("the pin's button is reachable")
        .get_rect();
    let host = harness.root::<Map>().host_of("place").expect("the pin");
    let at = host.transform * button_at.center();
    assert!(
        button_at.width() > 0.0,
        "the button has a box: {button_at:?}"
    );
    assert!(
        (at.x - button_at.center().x).abs() > 1.0 || (at.y - button_at.center().y).abs() > 1.0,
        "the camera drew the button elsewhere than its own space: {at:?} against {button_at:?}"
    );
    let _ = harness.dispatch_to_root(&crate::synthetic_input::cursor_moved(at.x, at.y));
    let _ = harness.dispatch_to_root(&crate::synthetic_input::mouse_press());
    let _ = harness.dispatch_to_root(&crate::synthetic_input::mouse_release());
    assert_eq!(
        clicks.get(),
        1,
        "a press at the button's scaled on-screen position pressed it"
    );
}

/// Draw `harness`'s root and return the scene it painted.
fn draw_root(harness: &mut crate::headless::FrameHarness) -> Scene {
    let mut scene = Scene::new();
    let theme = theme();
    let mut text = crate::font_catalog::fixture_renderer();
    let presence = PresenceCtx::default();
    let root = harness.root_widget_mut();
    root.draw(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
    scene
}

#[test]
fn panning_and_zooming_lay_no_pin_out_again() {
    let _frame = crate::motion::pin_frame();
    let mut map = Map::new().pin(
        "one",
        PinPlace {
            x: 100.0,
            y: 60.0,
            width: 200.0,
            height: Some(120.0),
        },
        Box::new(crate::widget::Spacer::new()),
    );
    let mut harness =
        crate::headless::FrameHarness::for_root(crate::State::new(), map, (800, 600), theme());
    harness.relayout();

    // The pin's view counts how often it is laid out.
    let before = harness
        .root::<Map>()
        .host_of("one")
        .expect("the pin")
        .layouts();
    assert!(before >= 1, "the pin was laid out at least once");

    let ctrl = winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
    for round in 1..=4 {
        let _ = harness.dispatch_to_root(&crate::synthetic_input::cursor_moved(400.0, 300.0));
        let _ =
            harness.dispatch_to_root_with(&crate::synthetic_input::mouse_wheel(0.0, 1.0), &ctrl);
        harness.relayout();
        // A middle drag pans the camera too.
        let _ = harness.dispatch_to_root(&crate::synthetic_input::cursor_moved(700.0, 500.0));
        let _ = harness.dispatch_to_root(&crate::synthetic_input::mouse_input(
            winit::event::ElementState::Pressed,
            winit::event::MouseButton::Middle,
        ));
        for step in 1..=4 {
            let fraction = f64::from(step) / 4.0;
            let _ = harness.dispatch_to_root(&crate::synthetic_input::cursor_moved(
                700.0 - 40.0 * fraction,
                500.0 - 30.0 * fraction,
            ));
        }
        let _ = harness.dispatch_to_root(&crate::synthetic_input::mouse_input(
            winit::event::ElementState::Released,
            winit::event::MouseButton::Middle,
        ));
        harness.relayout();
        let after = harness
            .root::<Map>()
            .host_of("one")
            .expect("the pin")
            .layouts();
        assert_eq!(
            after, before,
            "round {round}: a camera move laid the pin out again ({before} became {after})"
        );
    }
}
