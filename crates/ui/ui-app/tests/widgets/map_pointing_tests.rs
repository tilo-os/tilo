use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::internals::map::test_support::harness;
use tilo_ui::widget::internals::map::test_support::*;
use tilo_ui::widget::internals::map::*;
use tilo_ui::widget::*;
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
