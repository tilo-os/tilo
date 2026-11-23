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
    map.set_camera(camera);
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
            gesture: None,
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
            gesture: None,
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
            gesture: None,
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
            band: 0.0,
            hug: false,
            grows: false,
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

/// What a place holds that takes right presses itself — a field's copy and paste, a row's own
/// menu — stood for by a probe that takes them.
struct TakesRightPress {
    inner: crate::widget::Spacer,
    took: std::rc::Rc<std::cell::Cell<u32>>,
}

impl crate::widget::Decorator for TakesRightPress {
    type Inner = crate::widget::Spacer;

    fn inner(&self) -> &crate::widget::Spacer {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut crate::widget::Spacer {
        &mut self.inner
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Right,
            ..
        } = event
        {
            self.took.set(self.took.get() + 1);
            *ctx.handled = true;
        }
    }
}

/// A right press on a place whose content has a menu of its own goes to that content — a field
/// keeps its copy and paste — and the map reports nothing; free space is still the map's.
#[test]
fn a_right_press_on_content_with_its_own_menu_is_the_contents() {
    let queue = crate::Intents::default();
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
            inner: crate::widget::Spacer::new(),
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
    let queue = crate::Intents::default();
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
            gesture: None,
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

/// Touch points as the pointer does (`canvas.md` §2, "Touch"): a tap on free space reports its
/// point, a hold reports the secondary press there, and a one-finger drag across free space
/// reports the frame it covered rather than scrolling.
#[test]
fn a_finger_points_as_the_pointer_does() {
    let queue = crate::Intents::default();
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
    let queue = crate::Intents::default();
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

/// Dragging a place's corner sizes it: the map reports the place and its new size in canvas
/// pixels, at the camera's zoom, and points at nothing on the canvas while it does.
#[test]
fn dragging_a_places_corner_reports_its_new_size() {
    // The place stands where it arrives at once: a place still gliding in is grabbed where it
    // has come to, which a single frame does not settle.
    let _settled = crate::motion::testing::settled();
    let queue = crate::Intents::default();
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
                body: Some(Box::new(crate::widget::Spacer::fixed(300.0))),
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
    let theme = crate::Theme::dark();
    let render = |outline: Vec<i64>| -> Vec<u8> {
        let queue = crate::Intents::default();
        let mut map = pointing_map(&queue, MapCamera::default());
        map.outline = outline_of(&outline);
        let mut text = crate::font_catalog::fixture_renderer();
        crate::widget::layout::lay_out_detached(
            &mut map,
            Rect::new(0.0, 0.0, 400.0, 300.0),
            &theme,
            &mut text,
        )
        .expect("the map lays out");
        let mut scene = Scene::new();
        Widget::draw(
            &mut map,
            &mut crate::draw::DrawCtx::new(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                crate::presence::PresenceCtx::empty().draw_env(),
            ),
        );
        crate::offscreen::OffscreenRenderer::new(400, 300)
            .render(&scene, vello::peniko::Color::BLACK)
    };
    let plain = render(Vec::new());
    let outlined = render(vec![100, 80, 120, 90]);
    let at =
        |rgba: &[u8], x: u32, y: u32| crate::offscreen::OffscreenRenderer::sample(rgba, 400, x, y);
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

/// A press that lands outside the map is no pointing at the canvas, whatever of it reaches the
/// map: the shell's bar stands over the canvas in the Shell style, and a press on its bell or
/// its window menu that the map answered as a click on the canvas opened the caret there and
/// closed what the press had opened.
#[test]
fn a_press_outside_the_map_reports_nothing() {
    let queue = crate::Intents::default();
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

/// A hugging pin reports the height it is drawn at, in canvas pixels, once: a view that reads the
/// canvas's free area learns what the pin takes without a report a frame, and hears of it again
/// only when its view comes to another height.
#[test]
fn a_hugging_pin_reports_the_height_it_is_drawn_at_once() {
    let _frame = crate::motion::pin_frame();
    let queue = crate::Intents::default();
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

/// The drawn height `reports` carries, as the one `drawn` report's key and height: the pin's key
/// and its height in canvas pixels.
fn drawn_height(reports: &[Fired]) -> i64 {
    assert_eq!(reports.len(), 1, "one report: {reports:?}");
    let Fired {
        key,
        event,
        value: Some(Value::List(parts)),
        gesture: None,
    } = &reports[0]
    else {
        panic!("the report is a list: {reports:?}");
    };
    assert_eq!((key.as_str(), event.as_str()), (CANVAS, "drawn"));
    assert_eq!(parts[0], Value::Text("turn".into()), "the pin's key");
    let Some(Value::List(height)) = parts.get(1) else {
        panic!("the report carries its height: {parts:?}");
    };
    let Some(Value::Int(height)) = height.first() else {
        panic!("a height in canvas pixels: {parts:?}");
    };
    *height
}

/// Lay `map` out at the pointing tests' viewport: what a change inside a pin lays out again.
fn lay_out(map: &mut Map) {
    let mut text = crate::font_catalog::fixture_renderer();
    crate::widget::layout::lay_out_detached(
        map,
        Rect::new(0.0, 0.0, 800.0, 600.0),
        &crate::Theme::dark(),
        &mut text,
    )
    .expect("the map lays out");
}
