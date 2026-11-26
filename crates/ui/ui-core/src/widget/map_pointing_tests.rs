//! Pointing on a map (`docs/designs/app-language/canvas.md` §6, "Pointing"): a click on free
//! space reports the canvas point under it, a drag across free space reports the rectangle it
//! covered, and a right press reports the place under it and the canvas point. All of them are
//! in canvas pixels at the camera the map looks with, so each is right at any pan or zoom. A
//! press on a place is the place's, never the map's, and a middle drag pans.
use super::test_support::*;

use super::*;
use tilo_ui_base::{component::Fired, component::Intent, component::Value};

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
    map.resize = Event::View(Intent::new(&queue, CANVAS, "resize"));
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
        Box::new(crate::widget::CanvasPlace::from_props(
            "one",
            crate::widget::PlaceProps {
                name: Some("Inbox".into()),
                carries: Some("tl:mail:root".into()),
                body: Some(Box::new(tilo_ui_base::widget::Spacer::fixed(300.0))),
                ..crate::widget::PlaceProps::default()
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

/// The rectangle a view points at is drawn as the frame's outline in the theme's focus colour,
/// at the camera's pan and zoom, and a map pointing at none draws no outline.
#[test]
fn a_pointed_rectangle_is_outlined_where_it_stands() {
    let theme = tilo_ui_base::Theme::dark();
    let render = |outline: Vec<i64>| -> Vec<u8> {
        let queue = tilo_ui_base::Intents::default();
        let mut map = pointing_map(&queue, MapCamera::default());
        map.outline = outline_of(&outline);
        let mut text = tilo_ui_base::font_catalog::fixture_renderer();
        tilo_ui_base::widget::layout::lay_out_detached(
            &mut map,
            Rect::new(0.0, 0.0, 400.0, 300.0),
            &theme,
            &mut text,
        )
        .expect("the map lays out");
        let mut scene = Scene::new();
        Widget::draw(
            &mut map,
            &mut tilo_ui_base::draw::DrawCtx::new(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                tilo_ui_base::presence::PresenceCtx::empty().draw_env(),
            ),
        );
        tilo_ui_base::offscreen::OffscreenRenderer::new(400, 300)
            .render(&scene, vello::peniko::Color::BLACK)
    };
    let plain = render(Vec::new());
    let outlined = render(vec![100, 80, 120, 90]);
    let at = |rgba: &[u8], x: u32, y: u32| {
        tilo_ui_base::offscreen::OffscreenRenderer::sample(rgba, 400, x, y)
    };
    assert_ne!(
        at(&outlined, 160, 80),
        at(&plain, 160, 80),
        "the outline's top edge is drawn where the rectangle stands"
    );
    assert_eq!(
        at(&outlined, 160, 125),
        at(&plain, 160, 125),
        "inside the rectangle stays the canvas"
    );
}

/// A hugging pin reports the height it is drawn at, in canvas pixels, once: a view that reads the
/// canvas's free area learns what the pin takes without a report a frame, and hears of it again
/// only when its view comes to another height.
#[test]
fn a_hugging_pin_reports_the_height_it_is_drawn_at_once() {
    let _frame = tilo_ui_base::motion::pin_frame();
    let queue = tilo_ui_base::Intents::default();
    let mut map = pointing_map(&queue, MapCamera::default());
    map.drawn = Event::View(Intent::new(&queue, CANVAS, "drawn"));
    // A pin that hugs its words: as tall as they come to, up to its own height.
    map = map.pin(
        "turn",
        PinPlace {
            x: 0.0,
            y: 0.0,
            width: 400.0,
            height: Some(480.0),
            band: 0.0,
            hug: true,
            grows: true,
        },
        Box::new(crate::widget::Text::new("Here are your mails.")),
    );
    lay_out(&mut map);
    // The first solve reports the height the words come to, once.
    let first = reported(&queue);
    let height = drawn_height(&first);
    assert!(
        height > 0 && height < 480,
        "the turn hugs its words, under its own height: {height}"
    );
    // A frame that changes nothing reports nothing: the map rests.
    lay_out(&mut map);
    assert_eq!(reported(&queue), Vec::new(), "a still canvas says nothing");
    // The pin's view comes to another height: it is reported once more, taller.
    map.put_pin_view(
        0,
        Box::new(crate::widget::Text::new(
            "Here are your mails. This is a longer answer, so the card it stands in is taller \
             than the last one, and the height it is drawn at changes with it.",
        )),
    );
    lay_out(&mut map);
    let again = reported(&queue);
    let taller = drawn_height(&again);
    assert!(
        taller > height,
        "the taller answer is drawn taller: {taller} was {height}, {again:?}"
    );
    // And the frame after it says nothing, the change reported once.
    lay_out(&mut map);
    assert_eq!(reported(&queue), Vec::new(), "the change was reported once");
}

/// The camera comes to rest after a move of the person's and says where they left it: the place
/// at its middle, its zoom and the degrees at what it shows at its edges. A camera the view set
/// is no move of the person's, and reports nothing.
#[test]
fn the_camera_reports_where_the_person_left_it_when_it_comes_to_rest() {
    let queue: tilo_ui_base::Intents<Fired> = tilo_ui_base::Intents::default();
    let mut map = Map::new();
    map.rest = Event::View(Intent::new(&queue, CANVAS, "rest"));
    // The view opens the map on Munich.
    map.set_camera(MapCamera {
        origin: mercator_project(48.1374, 11.5755) - Vec2::new(400.0, 300.0),
        zoom: 2.0,
    });
    let mut host = laid_out(map);
    let _ = host.wait_until(crate::time::Duration::from_millis(300), |_| false);
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
    let _ = host.wait_until(crate::time::Duration::from_millis(600), |_| false);
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
    let mut host = world_harness(&queue, camera);
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
    let mut host = world_harness(&queue, camera);
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
    map.world = true;
    map.focus = Some((48.1374, 11.5755));
    map.at = Event::View(Intent::new(&queue, CANVAS, "at"));
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
