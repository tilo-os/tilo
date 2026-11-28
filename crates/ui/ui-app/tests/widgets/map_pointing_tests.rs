use crate::support::map::harness;
use crate::support::map::*;
use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::Event;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use tilo_ui_map::test_support::*;
use tilo_ui_map::*;
use winit::event::*;
use winit::keyboard::*;
/// A click on the empty canvas reports the canvas point under it, at the camera the map looks
/// with: the point the person pointed at, whatever the pan and the zoom.
#[test]
fn a_click_on_free_space_reports_its_canvas_point() {
    let queue = tilo_ui_base::Intents::default();
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
            gesture: None,
        }],
        "the click reports where it landed on the canvas"
    );
}

/// Two clicks at two different free points report two different canvas points: the map tells the
/// person's pointing apart, so the caret can stand at each.
#[test]
fn two_clicks_at_two_points_report_two_canvas_points() {
    let queue = tilo_ui_base::Intents::default();
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
                gesture: None,
            },
            Fired {
                key: CANVAS.into(),
                event: "press".into(),
                value: Some(Value::List(vec![Value::Int(610), Value::Int(470)])),
                gesture: None,
            },
        ],
        "each click reports its own point"
    );
}

/// A drag across the empty canvas reports the rectangle it covered, in canvas pixels, and not a
/// click: the person pointed at a frame, and the view lays what answers into it.
#[test]
fn a_drag_on_free_space_reports_the_rectangle_it_covered() {
    let queue = tilo_ui_base::Intents::default();
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
            gesture: None,
        }],
        "a drag reports the frame it covered, never a click"
    );
}

/// A drag down and to the leading side reports the same rectangle as one up and to the trailing
/// side: a frame is the space between two corners, whichever way the person drew it.
#[test]
fn a_drag_reports_its_frame_whichever_way_it_was_drawn() {
    let queue = tilo_ui_base::Intents::default();
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
            gesture: None,
        }],
        "the frame is the rectangle between the corners, drawn either way"
    );
}

/// A middle press pans the canvas and is no pointing at it, and a wheel over it is a zoom:
/// neither reports a click, a frame or a place.
#[test]
fn a_middle_press_and_a_wheel_report_nothing() {
    let queue = tilo_ui_base::Intents::default();
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

/// Touch points as the pointer does (`canvas.md` §2, "Touch"): a tap on free space reports its
/// point, a hold reports the secondary press there, and a one-finger drag across free space
/// reports the frame it covered rather than scrolling.
#[test]
fn a_finger_points_as_the_pointer_does() {
    let queue = tilo_ui_base::Intents::default();
    let camera = shifted();
    let mut host = harness(&queue, camera);

    let at = Point::new(400.0, 300.0);
    host.tap_at(at.x, at.y);
    let (x, y) = canvas_point(camera, at);
    assert_eq!(
        reported(&queue),
        [Fired {
            key: CANVAS.into(),
            event: "press".into(),
            value: Some(Value::List(vec![Value::Int(x), Value::Int(y)])),
            gesture: None,
        }],
        "a tap is a click on the canvas"
    );

    host.touch_hold_at(at.x, at.y);
    let fired = reported(&queue);
    assert!(
        fired.iter().any(|fired| fired.event == "context"),
        "a hold is the secondary press: {fired:?}"
    );

    let (from, to) = (Point::new(200.0, 150.0), Point::new(520.0, 430.0));
    host.swipe_at((from.x, from.y), (to.x, to.y));
    let fired = reported(&queue);
    let frames: Vec<&Fired> = fired
        .iter()
        .filter(|fired| fired.event == "frame")
        .collect();
    assert_eq!(
        frames.len(),
        1,
        "a drag across free space is one frame: {fired:?}"
    );
    let Some(Value::List(rect)) = &frames[0].value else {
        panic!("a frame carries its rectangle: {fired:?}");
    };
    let (fx, fy, fw, fh) = canvas_rect(camera, from, to);
    let near = |a: &Value, b: i64| matches!(a, Value::Int(v) if (v - b).abs() <= 24);
    assert!(
        near(&rect[0], fx) && near(&rect[1], fy) && near(&rect[2], fw) && near(&rect[3], fh),
        "the frame is the rectangle the finger covered, past the tap's slop: {rect:?} for {:?}",
        (fx, fy, fw, fh)
    );
}

/// Two fingers over free space pan the canvas and a pinch zooms it: the lens moves, and nothing is
/// pointed at.
#[test]
fn two_fingers_pan_and_a_pinch_zooms() {
    let queue = tilo_ui_base::Intents::default();
    let mut host = harness(&queue, MapCamera::default());
    let device = winit::event::DeviceId::dummy();
    host.dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: device,
        position: winit::dpi::PhysicalPosition::new(400.0, 300.0),
    });
    host.dispatch_to_root(&WindowEvent::MouseWheel {
        device_id: device,
        delta: winit::event::MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(
            -40.0, 25.0,
        )),
        phase: winit::event::TouchPhase::Moved,
    });
    host.dispatch_to_root(&WindowEvent::PinchGesture {
        device_id: device,
        delta: 0.25,
        phase: winit::event::TouchPhase::Moved,
    });
    let map = host
        .root_widget_mut()
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<Map>())
        .expect("the root is the map");
    let camera = map.camera();
    assert!(
        camera.origin != Point::ORIGIN,
        "two fingers panned the lens: {camera:?}"
    );
    assert!(camera.zoom > 1.0, "the pinch zoomed in: {camera:?}");
    assert_eq!(reported(&queue), Vec::new(), "nothing was pointed at");
}

/// A press that lands outside the map is no pointing at the canvas, whatever of it reaches the
/// map: the shell's bar stands over the canvas in the Shell style, and a press on its bell or
/// its window menu that the map answered as a click on the canvas opened the caret there and
/// closed what the press had opened.
#[test]
fn a_press_outside_the_map_reports_nothing() {
    let queue = tilo_ui_base::Intents::default();
    let mut host = harness(&queue, MapCamera::default());
    let device = winit::event::DeviceId::dummy();
    host.dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: device,
        position: winit::dpi::PhysicalPosition::new(900.0, 300.0),
    });
    for button in [MouseButton::Left, MouseButton::Right] {
        for state in [ElementState::Pressed, ElementState::Released] {
            host.dispatch_to_root(&WindowEvent::MouseInput {
                device_id: device,
                state,
                button,
            });
        }
    }
    assert_eq!(
        reported(&queue),
        Vec::new(),
        "a press beside the map is not the canvas's"
    );
}

/// A right press on the empty canvas reports no place and the canvas point under it; a right
/// press on a place reports that place's key: what the person pointed at.
#[test]
fn a_right_press_reports_the_place_under_it_and_the_canvas_point() {
    let queue = tilo_ui_base::Intents::default();
    let camera = MapCamera::default();
    let mut map = pointing_map(&queue, camera);
    map = map.pin(
        "one",
        PinPlace {
            x: 100.0,
            y: 100.0,
            width: 240.0,
            height: Some(200.0),
            band: 0.0,
            hug: false,
            grows: false,
        },
        Box::new(tilo_ui_base::widget::Spacer::new()),
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
            gesture: None,
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
            gesture: None,
        }],
        "a right press on a place names it"
    );
}

/// A right press on a place whose content has a menu of its own goes to that content — a field
/// keeps its copy and paste — and the map reports nothing; free space is still the map's.
#[test]
fn a_right_press_on_content_with_its_own_menu_is_the_contents() {
    let queue = tilo_ui_base::Intents::default();
    let took = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut map = pointing_map(&queue, MapCamera::default());
    map = map.pin(
        "field",
        PinPlace {
            x: 100.0,
            y: 100.0,
            width: 240.0,
            height: Some(200.0),
            band: 0.0,
            hug: false,
            grows: false,
        },
        Box::new(TakesRightPress {
            inner: tilo_ui_base::widget::Spacer::new(),
            took: std::rc::Rc::clone(&took),
        }),
    );
    let mut host = laid_out(map);
    right_click(&mut host, Point::new(220.0, 200.0));
    assert_eq!(took.get(), 1, "the content took its right press");
    assert_eq!(
        reported(&queue),
        Vec::new(),
        "the map reports no pointing for a press its content took"
    );
    right_click(&mut host, Point::new(650.0, 500.0));
    assert_eq!(
        reported(&queue).len(),
        1,
        "a right press on free space is still the map's"
    );
}

/// A left press on a place is the place's, not the canvas's: the map never reports a click or a
/// frame for a press that landed on a pin.
#[test]
fn a_press_on_a_place_is_not_pointing_at_the_canvas() {
    let queue = tilo_ui_base::Intents::default();
    let mut map = pointing_map(&queue, MapCamera::default());
    map = map.pin(
        "one",
        PinPlace {
            x: 100.0,
            y: 100.0,
            width: 240.0,
            height: Some(200.0),
            band: 0.0,
            hug: false,
            grows: false,
        },
        Box::new(tilo_ui_base::widget::Spacer::new()),
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
            gesture: None,
        }],
        "the empty canvas around the place still reports its pointing"
    );
}

/// Dragging a place's corner sizes it: the map reports the place and its new size in canvas
/// pixels, at the camera's zoom, and points at nothing on the canvas while it does.
#[test]
fn dragging_a_places_corner_reports_its_new_size() {
    // The place stands where it arrives at once: a place still gliding in is grabbed where it
    // has come to, which a single frame does not settle.
    let _settled = tilo_ui_base::motion::testing::settled();
    let queue = tilo_ui_base::Intents::default();
    let camera = MapCamera {
        origin: Point::ORIGIN,
        zoom: 2.0,
    };
    let mut map = pointing_map(&queue, camera);
    map = reports(map, &queue, "resize");
    map = map.pin(
        "one",
        PinPlace {
            x: 50.0,
            y: 50.0,
            width: 200.0,
            height: Some(150.0),
            band: 0.0,
            hug: false,
            grows: false,
        },
        // A place as the canvas stands it: its view on the surface, its handle and its menu.
        Box::new(tilo_ui_app::widget::CanvasPlace::from_props(
            "one",
            tilo_ui_app::widget::PlaceProps {
                name: Some("Inbox".into()),
                carries: Some("tl:mail:root".into()),
                body: Some(Box::new(tilo_ui_base::widget::Spacer::fixed(300.0))),
                ..tilo_ui_app::widget::PlaceProps::default()
            },
        )),
    );
    let mut host = laid_out(map);
    // The pin stands at (100, 100) to (500, 400) on screen at zoom 2: its corner is (500, 400).
    host.drag_at((500.0, 400.0), (580.0, 460.0));
    assert_eq!(
        reported(&queue),
        [Fired {
            key: CANVAS.into(),
            event: "resize".into(),
            value: Some(Value::List(vec![
                Value::Text("one".into()),
                Value::List(vec![Value::Int(240), Value::Int(180)]),
            ])),
            gesture: None,
        }],
        "the corner's drag sizes the place by what it travelled, in canvas pixels"
    );
}

/// The camera comes to rest after a move of the person's and says where they left it: the place
/// at its middle, its zoom and the degrees at what it shows at its edges. A camera the view set
/// is no move of the person's, and reports nothing.
#[test]
fn the_camera_reports_where_the_person_left_it_when_it_comes_to_rest() {
    let queue: tilo_ui_base::Intents<Fired> = tilo_ui_base::Intents::default();
    let mut map = Map::new();
    map = reports(map, &queue, "rest");
    // The view opens the map on Munich.
    map.set_camera(MapCamera {
        origin: mercator_project(48.1374, 11.5755) - Vec2::new(400.0, 300.0),
        zoom: 2.0,
    });
    let mut host = laid_out(map);
    let _ = host.wait_until(tilo_ui_app::time::Duration::from_millis(300), |_| false);
    assert!(
        reported(&queue).is_empty(),
        "a centre the view set is not a rest of the person's"
    );
    // A wheel over free space pans the lens; once it stands still it reports where it looks.
    host.dispatch_to_root(&WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: winit::event::MouseScrollDelta::LineDelta(0.0, 1.0),
        phase: winit::event::TouchPhase::Moved,
    });
    let _ = host.wait_until(tilo_ui_app::time::Duration::from_millis(600), |_| false);
    let fired = reported(&queue);
    let rest = fired
        .iter()
        .find(|fired| fired.event == "rest")
        .unwrap_or_else(|| panic!("the camera at rest reports where it looks: {fired:?}"));
    let Some(Value::List(values)) = &rest.value else {
        panic!("a rest carries its place: {fired:?}");
    };
    let floats: Vec<f64> = values
        .iter()
        .map(|value| match value {
            Value::Float(number) => *number,
            other => panic!("a rest is read as numbers: {other:?}"),
        })
        .collect();
    assert_eq!(
        floats.len(),
        7,
        "a place, a zoom and four edges: {floats:?}"
    );
    let map = host
        .root_widget_mut()
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<Map>())
        .expect("the root is the map");
    let camera = map.camera();
    let (latitude, longitude) =
        mercator_unproject(camera.to_canvas(Point::ORIGIN, Point::new(400.0, 300.0)));
    assert!(
        (floats[0] - latitude).abs() < 1e-6 && (floats[1] - longitude).abs() < 1e-6,
        "the place at the middle is the one it shows: {floats:?} for {latitude}, {longitude}"
    );
    assert!((floats[2] - camera.zoom).abs() < 1e-9, "its own zoom");
    assert!(
        floats[3] > floats[4] && floats[5] > floats[6],
        "the northern edge is above the southern, the eastern east of the western: {floats:?}"
    );
    assert!(
        (floats[0] - 48.1374).abs() < 0.2 && (floats[1] - 11.5755).abs() < 0.2,
        "one notch of a wheel over Munich stays over Munich: {floats:?}"
    );
}

/// A wheel over a map of the world zooms the map about the pointer, keeping the place under the
/// pointer where it is: a map is read by zooming, not scrolled.
#[test]
fn a_wheel_over_a_map_of_the_world_zooms_about_the_pointer() {
    let queue = tilo_ui_base::Intents::default();
    let camera = MapCamera {
        origin: Point::new(100.0, 50.0),
        zoom: 0.02,
    };
    let mut host = laid_out(world_map(&queue, camera));
    let at = Point::new(500.0, 200.0);
    let device = winit::event::DeviceId::dummy();
    host.dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: device,
        position: winit::dpi::PhysicalPosition::new(at.x, at.y),
    });
    host.dispatch_to_root(&WindowEvent::MouseWheel {
        device_id: device,
        delta: winit::event::MouseScrollDelta::LineDelta(0.0, 1.0),
        phase: winit::event::TouchPhase::Moved,
    });
    let after = map_camera(&mut host);
    assert!(
        after.zoom > camera.zoom * 1.05,
        "the wheel zoomed the map in: {after:?}"
    );
    let held = camera.to_canvas(Point::ORIGIN, at);
    let still = after.to_canvas(Point::ORIGIN, at);
    assert!(
        (held - still).hypot() < 0.5,
        "the place under the pointer stayed where it was: {held:?} then {still:?}"
    );
    assert_eq!(reported(&queue), Vec::new(), "zooming points at nothing");
}

/// Dragging a map of the world pans it: the map moves under the hand, and a drag is never a
/// rectangle the person pointed with.
#[test]
fn dragging_a_map_of_the_world_pans_it() {
    let queue = tilo_ui_base::Intents::default();
    let camera = MapCamera {
        origin: Point::new(20.0, 30.0),
        zoom: 0.05,
    };
    let mut host = laid_out(world_map(&queue, camera));
    let device = winit::event::DeviceId::dummy();
    let from = Point::new(300.0, 200.0);
    let to = Point::new(460.0, 280.0);
    host.dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: device,
        position: winit::dpi::PhysicalPosition::new(from.x, from.y),
    });
    host.dispatch_to_root(&WindowEvent::MouseInput {
        device_id: device,
        state: ElementState::Pressed,
        button: MouseButton::Left,
    });
    host.dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: device,
        position: winit::dpi::PhysicalPosition::new(to.x, to.y),
    });
    host.dispatch_to_root(&WindowEvent::MouseInput {
        device_id: device,
        state: ElementState::Released,
        button: MouseButton::Left,
    });
    let after = map_camera(&mut host);
    let travelled = to - from;
    let expected = camera.origin - travelled / camera.zoom;
    assert!(
        (after.origin - expected).hypot() < 0.5,
        "the map followed the hand: {:?} for {:?}",
        after.origin,
        expected
    );
    assert_eq!(after.zoom, camera.zoom, "a drag does not zoom the map");
    assert_eq!(
        reported(&queue),
        Vec::new(),
        "a drag across a map of the world points at nothing"
    );
}

/// A click on a map of the world — a press let go where it landed — reports the place under it,
/// so the view can open what stands there.
#[test]
fn a_click_on_a_map_of_the_world_reports_the_place_under_it() {
    let queue = tilo_ui_base::Intents::default();
    let zoom = 0.05;
    let centre = mercator_project(48.1374, 11.5755);
    let origin = centre - Vec2::new(400.0, 300.0) / zoom;
    let mut map = pointing_map(&queue, MapCamera { origin, zoom });
    map = of_the_world(map, Some((48.1374, 11.5755)));
    map = reports(map, &queue, "at");
    let mut host = laid_out(map);
    host.click_at(400.0, 300.0);
    let fired = reported(&queue);
    let Some(Value::List(at)) = fired.first().and_then(|fired| fired.value.as_ref()) else {
        panic!("the click reported no place: {fired:?}");
    };
    let near =
        |value: &Value, want: f64| matches!(value, Value::Float(v) if (v - want).abs() < 0.001);
    assert!(
        near(&at[0], 48.1374) && near(&at[1], 11.5755),
        "the click reported the place it landed on, in degrees: {at:?}"
    );
}
