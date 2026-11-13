//! Pointing on a map (`docs/designs/app-language/canvas.md` §6, "Pointing"): a click on free
//! space reports the canvas point under it, a drag across free space reports the rectangle it
//! covered, and a right press reports the place under it and the canvas point. All of them are
//! in canvas pixels at the camera the map looks with, so each is right at any pan or zoom. A
//! press on a place is the place's, never the map's, and a middle drag pans.

use super::*;
use crate::Theme;
use crate::component::{Fired, Intent, Value};
use crate::headless::FrameHarness;

/// The key the map reports its pointing under.
const CANVAS: &str = "canvas";

/// A map of no places that reports what pointing at it does on `queue`, looking with `camera`.
fn pointing_map(queue: &crate::Intents<Fired>, camera: MapCamera) -> Map {
    let mut map = Map::new();
    map.press = Event::View(Intent::new(queue, CANVAS, "press"));
    map.frame = Event::View(Intent::new(queue, CANVAS, "frame"));
    map.context = Event::View(Intent::new(queue, CANVAS, "context"));
    map.camera = camera;
    map
}

/// A map laid out over the whole viewport, with its camera as given: the camera is the map's
/// own, kept across the layout, as a live frame leaves it.
fn laid_out(mut map: Map) -> FrameHarness {
    let mut text = crate::font_catalog::fixture_renderer();
    crate::widget::layout::lay_out_detached(
        &mut map,
        Rect::new(0.0, 0.0, 800.0, 600.0),
        &Theme::dark(),
        &mut text,
    )
    .expect("the map lays out");
    FrameHarness::for_root(crate::State::new(), map, (800, 600), Theme::dark())
}

/// [`laid_out`] of a map with no places, looking with `camera`.
fn harness(queue: &crate::Intents<Fired>, camera: MapCamera) -> FrameHarness {
    laid_out(pointing_map(queue, camera))
}

/// A camera neither at the origin nor at zoom 1: a pan and a zoom the person might leave the
/// canvas looking through, so a point read back at it proves the camera is taken into account.
fn shifted() -> MapCamera {
    MapCamera {
        origin: Point::new(150.0, -60.0),
        zoom: 1.6,
    }
}

/// The canvas point `screen` stands at, under `camera`, on a map whose surface starts at the
/// viewport's own corner.
fn canvas_point(camera: MapCamera, screen: Point) -> (i64, i64) {
    let at = camera.to_canvas(Point::ORIGIN, screen);
    (at.x.round() as i64, at.y.round() as i64)
}

/// The canvas rectangle between two screen points, in canvas pixels: the drag's frame.
fn canvas_rect(camera: MapCamera, from: Point, to: Point) -> (i64, i64, i64, i64) {
    let a = camera.to_canvas(Point::ORIGIN, from);
    let b = camera.to_canvas(Point::ORIGIN, to);
    let rect = Rect::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y));
    (
        rect.x0.round() as i64,
        rect.y0.round() as i64,
        rect.width().round() as i64,
        rect.height().round() as i64,
    )
}

/// The pointing the queue reported.
fn reported(queue: &crate::Intents<Fired>) -> Vec<Fired> {
    queue.take()
}

/// Press the map's right button at `at`, with the pointer there.
fn right_click(host: &mut FrameHarness, at: Point) {
    let device = winit::event::DeviceId::dummy();
    host.dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: device,
        position: winit::dpi::PhysicalPosition::new(at.x, at.y),
    });
    for state in [ElementState::Pressed, ElementState::Released] {
        host.dispatch_to_root(&WindowEvent::MouseInput {
            device_id: device,
            state,
            button: MouseButton::Right,
        });
    }
}

/// A click on the empty canvas reports the canvas point under it, at the camera the map looks
/// with: the point the person pointed at, whatever the pan and the zoom.
#[test]
fn a_click_on_free_space_reports_its_canvas_point() {
    let queue = crate::Intents::default();
    let camera = shifted();
    let mut host = harness(&queue, camera);
    let at = Point::new(400.0, 300.0);
    host.click_at(at.x, at.y);
    let (x, y) = canvas_point(camera, at);
    assert_eq!(
        reported(&queue),
        [Fired {
            key: CANVAS.into(),
            event: "press".into(),
            value: Some(Value::List(vec![Value::Int(x), Value::Int(y)])),
        }],
        "the click reports where it landed on the canvas"
    );
}

/// Two clicks at two different free points report two different canvas points: the map tells the
/// person's pointing apart, so the caret can stand at each.
#[test]
fn two_clicks_at_two_points_report_two_canvas_points() {
    let queue = crate::Intents::default();
    let camera = MapCamera::default();
    let mut host = harness(&queue, camera);
    host.click_at(120.0, 90.0);
    host.click_at(610.0, 470.0);
    assert_eq!(
        reported(&queue),
        [
            Fired {
                key: CANVAS.into(),
                event: "press".into(),
                value: Some(Value::List(vec![Value::Int(120), Value::Int(90)])),
            },
            Fired {
                key: CANVAS.into(),
                event: "press".into(),
                value: Some(Value::List(vec![Value::Int(610), Value::Int(470)])),
            },
        ],
        "each click reports its own point"
    );
}

/// A drag across the empty canvas reports the rectangle it covered, in canvas pixels, and not a
/// click: the person pointed at a frame, and the view lays what answers into it.
#[test]
fn a_drag_on_free_space_reports_the_rectangle_it_covered() {
    let queue = crate::Intents::default();
    let camera = shifted();
    let mut host = harness(&queue, camera);
    let (from, to) = (Point::new(200.0, 180.0), Point::new(560.0, 460.0));
    host.drag_at((from.x, from.y), (to.x, to.y));
    let (x, y, width, height) = canvas_rect(camera, from, to);
    assert_eq!(
        reported(&queue),
        [Fired {
            key: CANVAS.into(),
            event: "frame".into(),
            value: Some(Value::List(vec![
                Value::Int(x),
                Value::Int(y),
                Value::Int(width),
                Value::Int(height),
            ])),
        }],
        "a drag reports the frame it covered, never a click"
    );
}

/// A drag down and to the leading side reports the same rectangle as one up and to the trailing
/// side: a frame is the space between two corners, whichever way the person drew it.
#[test]
fn a_drag_reports_its_frame_whichever_way_it_was_drawn() {
    let queue = crate::Intents::default();
    let camera = MapCamera::default();
    let mut host = harness(&queue, camera);
    host.drag_at((600.0, 500.0), (300.0, 200.0));
    assert_eq!(
        reported(&queue),
        [Fired {
            key: CANVAS.into(),
            event: "frame".into(),
            value: Some(Value::List(vec![
                Value::Int(300),
                Value::Int(200),
                Value::Int(300),
                Value::Int(300),
            ])),
        }],
        "the frame is the rectangle between the corners, drawn either way"
    );
}

/// A right press on the empty canvas reports no place and the canvas point under it; a right
/// press on a place reports that place's key: what the person pointed at.
#[test]
fn a_right_press_reports_the_place_under_it_and_the_canvas_point() {
    let queue = crate::Intents::default();
    let camera = MapCamera::default();
    let mut map = pointing_map(&queue, camera);
    map = map.pin(
        "one",
        PinPlace {
            x: 100.0,
            y: 100.0,
            width: 240.0,
            height: Some(200.0),
        },
        Box::new(crate::widget::Spacer::new()),
    );
    let mut host = laid_out(map);
    // Free space: no place, and the point under the pointer.
    let at = Point::new(650.0, 500.0);
    right_click(&mut host, at);
    let (x, y) = canvas_point(camera, at);
    assert_eq!(
        reported(&queue),
        [Fired {
            key: CANVAS.into(),
            event: "context".into(),
            value: Some(Value::List(vec![
                Value::Text(String::new()),
                Value::List(vec![Value::Int(x), Value::Int(y)]),
            ])),
        }],
        "a right press on free space names no place"
    );
    // On the place: its key, and where the pointer stood.
    let at = Point::new(220.0, 200.0);
    right_click(&mut host, at);
    let (x, y) = canvas_point(camera, at);
    assert_eq!(
        reported(&queue),
        [Fired {
            key: CANVAS.into(),
            event: "context".into(),
            value: Some(Value::List(vec![
                Value::Text("one".into()),
                Value::List(vec![Value::Int(x), Value::Int(y)]),
            ])),
        }],
        "a right press on a place names it"
    );
}

/// A left press on a place is the place's, not the canvas's: the map never reports a click or a
/// frame for a press that landed on a pin.
#[test]
fn a_press_on_a_place_is_not_pointing_at_the_canvas() {
    let queue = crate::Intents::default();
    let mut map = pointing_map(&queue, MapCamera::default());
    map = map.pin(
        "one",
        PinPlace {
            x: 100.0,
            y: 100.0,
            width: 240.0,
            height: Some(200.0),
        },
        Box::new(crate::widget::Spacer::new()),
    );
    let mut host = laid_out(map);
    // A click in the middle of the place.
    host.click_at(220.0, 200.0);
    // A drag that starts on it and leaves it.
    host.drag_at((220.0, 200.0), (600.0, 480.0));
    assert_eq!(
        reported(&queue),
        Vec::new(),
        "a press that starts on a place is that place's"
    );
    // Off the place, the canvas answers again.
    host.click_at(700.0, 540.0);
    assert_eq!(
        reported(&queue),
        [Fired {
            key: CANVAS.into(),
            event: "press".into(),
            value: Some(Value::List(vec![Value::Int(700), Value::Int(540)])),
        }],
        "the empty canvas around the place still reports its pointing"
    );
}

/// A middle press pans the canvas and is no pointing at it, and a wheel over it is a zoom:
/// neither reports a click, a frame or a place.
#[test]
fn a_middle_press_and_a_wheel_report_nothing() {
    let queue = crate::Intents::default();
    let mut host = harness(&queue, MapCamera::default());
    let device = winit::event::DeviceId::dummy();
    host.dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: device,
        position: winit::dpi::PhysicalPosition::new(300.0, 300.0),
    });
    for state in [ElementState::Pressed, ElementState::Released] {
        host.dispatch_to_root(&WindowEvent::MouseInput {
            device_id: device,
            state,
            button: MouseButton::Middle,
        });
    }
    assert_eq!(reported(&queue), Vec::new(), "a middle press pans");
}
