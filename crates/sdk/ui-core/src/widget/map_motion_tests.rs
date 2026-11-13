//! A map's pins on their way (`motion::Placed`): a pin the person moves glides from where it
//! stood to its new place over the frames between, reaches it, and then rests asking for no
//! frames; a still theme and reduced motion land the pin at once.

use super::*;
use crate::Theme;

fn theme() -> Theme {
    Theme::dark()
}

/// A map of pins, not laid out yet: at its first layout it takes up the lens of a map dropped
/// in its place.
fn pins_of(pins: &[(&str, f64)]) -> Map {
    let mut map = Map::new();
    for (key, x) in pins {
        map = map.pin(
            *key,
            PinPlace {
                x: *x,
                y: 0.0,
                width: 200.0,
                height: Some(120.0),
            },
            Box::new(crate::widget::Spacer::new()),
        );
    }
    map
}

/// A map of pins, laid out in a viewport.
fn map_of(pins: &[(&str, f64)], viewport: Rect) -> Map {
    let mut map = pins_of(pins);
    lay_out_again(&mut map, viewport);
    map
}

/// Lay `map` out again: what a camera pan, a pin moved or a viewport change asks of its region.
fn lay_out_again(map: &mut Map, viewport: Rect) {
    let mut text = crate::font_catalog::fixture_renderer();
    crate::widget::layout::lay_out_detached(map, viewport, &theme(), &mut text)
        .expect("the map lays out");
}

/// A map drawn once, so its pins' motion begins where the region laid them out.
fn draw(map: &mut Map) {
    let mut scene = Scene::new();
    let mut text = crate::font_catalog::fixture_renderer();
    map.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &theme(),
        &PresenceCtx::default(),
    );
}

#[test]
fn a_pin_at_its_first_frame_rests_and_asks_for_no_frames() {
    let _frame = crate::motion::pin_frame();
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let mut map = map_of(&[("one", 100.0)], viewport);
    // The first draw places it: a pin the map shows at its first frame rests.
    draw(&mut map);
    let now = crate::motion::frame_now();
    assert!(
        map.placed
            .borrow()
            .get("one")
            .is_some_and(|p| !p.is_moving(now))
    );
    assert!(!map.pins_moving(now));
    assert!(
        !Widget::needs_animation_frame(&map),
        "a pin at rest asks for nothing"
    );
}

#[test]
fn a_pin_moved_by_the_person_glides_to_its_new_place_and_then_rests() {
    let _frame = crate::motion::pin_frame();
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let mut map = map_of(&[("one", 100.0)], viewport);
    draw(&mut map);
    let now = crate::motion::frame_now();
    assert!(map.placed.borrow().contains_key("one"), "the pin is drawn");

    // The person moves the pin: the map lays it out at its new point and draws again.
    crate::motion::note_source(crate::tokens::MotionSource::Pointer);
    map.pins[0].1.x = 400.0;
    map.invalidation.invalidate();
    lay_out_again(&mut map, viewport);
    draw(&mut map);
    assert!(
        map.placed
            .borrow()
            .get("one")
            .is_some_and(|p| p.movement().is_moving(now)),
        "the pin's move began: {:?}",
        map.placed
            .borrow()
            .get("one")
            .map(|p| p.movement().target())
    );
    let start = map
        .placed
        .borrow()
        .get("one")
        .map(|p| p.movement().sampled_after(crate::time::Duration::ZERO).0);
    let mid = map.placed.borrow().get("one").map(|p| {
        p.movement()
            .sampled_after(crate::time::Duration::from_millis(80))
            .0
    });
    let arrived = map.placed.borrow().get("one").map(|p| {
        p.movement()
            .sampled_after(crate::time::Duration::from_secs(3))
            .0
    });
    assert!(
        start.is_some_and(|r| (r.x0 - 100.0).abs() < 3.0),
        "it starts where it stood: {start:?}"
    );
    assert!(
        mid.is_some_and(|r| r.x0 > 101.0 && r.x0 < 399.0),
        "it is on its way: {mid:?}"
    );
    assert!(
        arrived.is_some_and(|r| (r.x0 - 400.0).abs() < 0.5),
        "it arrives: {arrived:?}"
    );
    assert!(
        !map.pins_moving(now + crate::time::Duration::from_secs(3)),
        "and then asks for no frames"
    );
}

#[test]
fn a_pin_shown_by_a_still_theme_lands_at_once() {
    let _frame = crate::motion::pin_frame();
    let still = crate::tokens::EffectiveMotion::new(
        crate::tokens::Motion {
            still: true,
            ..crate::tokens::Motion::default()
        },
        false,
    );
    let now = crate::motion::frame_now();
    // Show the pin at its place under a still theme: it lands at once and asks for nothing.
    let mut placed = crate::motion::Placed::closed();
    placed.follow(
        Rect::new(100.0, 0.0, 300.0, 120.0),
        true,
        still,
        crate::tokens::MotionSource::Pointer,
        now,
    );
    assert_eq!(placed.progress(now), 1.0, "shown on the frame it happens");
    assert!(!placed.is_moving(now), "and it asks for nothing");
}

#[test]
fn a_map_built_again_keeps_the_camera_and_the_places_where_they_were_drawn() {
    let _frame = crate::motion::pin_frame();
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let mut old = map_of(&[("one", 100.0)], viewport);
    draw(&mut old);
    // The person pans the canvas and leaves it there; the places on it change and the map is built
    // again around them, as a window opening on the canvas does.
    old.set_camera(MapCamera {
        origin: Point::new(40.0, 20.0),
        zoom: 1.0,
    });
    lay_out_again(&mut old, viewport);
    draw(&mut old);
    let left = old.camera();
    let mut map = pins_of(&[("one", 100.0)]);
    drop(old);
    lay_out_again(&mut map, viewport);
    draw(&mut map);
    assert!(
        map.camera().same(left),
        "the lens the person left the canvas at"
    );
    let now = crate::motion::frame_now();
    assert!(
        map.placed
            .borrow()
            .get("one")
            .is_some_and(|p| !p.is_moving(now)),
        "a place the map already held does not arrive again"
    );
    assert!(
        map.reveal.is_none(),
        "nothing arrived: the camera shows nothing new"
    );
}

#[test]
fn a_place_arriving_in_view_leaves_the_camera_where_the_person_left_it() {
    let _frame = crate::motion::pin_frame();
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let mut old = map_of(&[("one", 0.0)], viewport);
    draw(&mut old);
    let mut map = pins_of(&[("one", 0.0), ("two", 300.0)]);
    drop(old);
    lay_out_again(&mut map, viewport);
    lay_out_again(&mut map, viewport);
    assert!(
        map.camera().same(MapCamera::default()),
        "a place already in view moves nothing: {:?}",
        map.camera().origin
    );
}

#[test]
fn the_camera_glides_to_a_place_arriving_past_the_edge_laying_every_place_out_on_its_way() {
    let start = crate::time::now();
    let viewport = Rect::new(0.0, 0.0, 1200.0, 600.0);
    let (old, mut map) = {
        let _frame = crate::motion::pin_frame_at(start);
        let mut old = map_of(&[("one", 300.0)], viewport);
        draw(&mut old);
        (old, pins_of(&[("one", 300.0), ("two", 1100.0)]))
    };
    // The person opened it: the camera glides there, whoever's input came last.
    crate::motion::note_source(crate::tokens::MotionSource::Pointer);
    {
        let _frame = crate::motion::pin_frame_at(start);
        drop(old);
        lay_out_again(&mut map, viewport);
        draw(&mut map);
    }
    let doc = automerge::AutoCommit::new();
    let mut along = Vec::new();
    let mut frame = start;
    for _ in 0..120 {
        frame += crate::time::Duration::from_nanos(16_666_667);
        let _frame = crate::motion::pin_frame_at(frame);
        if !Widget::needs_animation_frame(&map) {
            break;
        }
        // A frame: the motion steps in the update, and the layout lays the places out there.
        Widget::update(&mut map, &doc, &automerge::ROOT);
        lay_out_again(&mut map, viewport);
        draw(&mut map);
        let laid = map.laid.borrow().clone();
        along.push((laid["one"].x0, laid["two"].x0));
    }
    assert!(
        along
            .iter()
            .all(|(one, two)| (two - one - 800.0).abs() < 0.5),
        "every place is laid out where the camera has carried it, each frame: {along:?}"
    );
    let (_, last) = *along.last().expect("the camera moved");
    assert!(
        (last - (1200.0 - 200.0 - ARRIVAL_MARGIN)).abs() < 0.5,
        "it arrives whole in view: {along:?}"
    );
    assert!(
        along
            .iter()
            .any(|(_, two)| *two < 1099.0 && *two > last + 1.0),
        "on its way over the frames between: {along:?}"
    );
    let _frame = crate::motion::pin_frame_at(frame);
    assert!(
        (map.camera().origin.x - (100.0 + ARRIVAL_MARGIN)).abs() < 0.5
            && !Widget::needs_animation_frame(&map),
        "the least pan that shows the whole place, and then it asks for no frames"
    );
    let places = map.canvas_places();
    assert_eq!(
        (
            places.get("one").map(|p| p.x),
            places.get("two").map(|p| p.x)
        ),
        (Some(300.0), Some(1100.0)),
        "no place on the canvas moved: only the lens did"
    );
}

/// A map of pins at `(key, x, y)`, not laid out yet: built in place of the one dropped before it.
fn pins_at(pins: &[(&str, f64, f64)]) -> Map {
    let mut map = Map::new();
    for (key, x, y) in pins {
        map = map.pin(
            *key,
            PinPlace {
                x: *x,
                y: *y,
                width: 200.0,
                height: Some(120.0),
            },
            Box::new(crate::widget::Spacer::new()),
        );
    }
    map
}

#[test]
fn a_map_is_known_by_the_key_its_view_gave_it_not_by_where_it_stands() {
    use crate::component::{Described, Event};
    // Two maps at the same origin are two canvases, and a drag let go over one must never land
    // on the other. The map's key on the drag bus is the stable one the view named it by, never
    // a place it happens to stand at: two maps' plates side by side at the same x would share a
    // key, and one would take the other's drop.
    let left = Map::from_props(
        "left",
        MapProps {
            drop: Event::None,
            ..MapProps::default()
        },
    );
    let right = Map::from_props(
        "right",
        MapProps {
            drop: Event::None,
            ..MapProps::default()
        },
    );
    assert_ne!(
        left.drop_key(),
        right.drop_key(),
        "two maps are told apart by their views' keys"
    );
    assert!(
        left.drop_key().contains("left"),
        "the left map is known by its own key: {}",
        left.drop_key()
    );
}

#[test]
fn a_place_that_arrives_off_screen_is_panned_into_view() {
    // The conversation stands at the origin and the viewport shows it; a place opened beside it
    // one row down — the row `free_place_next_to` puts below a taken column — arrives outside what
    // the map shows. The map pans so the person sees what was just placed, and never changes its
    // zoom: opening a place never resizes the places already there, so the conversation keeps the
    // size the person was reading it at, even when the pan brings the new place to the front and
    // the conversation goes partly out of sight (the person pans back to it).
    let _frame = crate::motion::pin_frame();
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let mut old = map_of(&[("conversation", 0.0)], viewport);
    draw(&mut old);
    let before = old.camera();
    let mut map = pins_at(&[("conversation", 0.0, 0.0), ("answer", 0.0, 544.0)]);
    drop(old);
    lay_out_again(&mut map, viewport);
    let after = map.camera();
    assert!(
        after.origin != before.origin,
        "the camera panned to show the place that arrived: {after:?} against {before:?}"
    );
    assert!(
        (after.zoom - before.zoom).abs() <= f64::EPSILON,
        "the camera kept its zoom — opening a place never resizes the places there: {after:?} \
         against {before:?}"
    );
    // What arrived stands wholly in view: the person sees what was just placed.
    let arrived = Rect::from_origin_size(
        after.to_screen(viewport.origin(), Point::new(0.0, 544.0)),
        (200.0 * after.zoom, 120.0 * after.zoom),
    );
    assert!(
        arrived.y0 >= viewport.y0 && arrived.y1 <= viewport.y1,
        "the place that arrived stands in view: {arrived:?} in {viewport:?}"
    );
    // And the conversation keeps the size it had: the camera panned, it never zoomed it.
    let conversation = Rect::from_origin_size(
        after.to_screen(viewport.origin(), Point::new(0.0, 0.0)),
        (200.0 * after.zoom, 120.0 * after.zoom),
    );
    assert!(
        (conversation.width() - 200.0 * before.zoom).abs() < 0.5,
        "the conversation is drawn at the width it was, never shrunk by what arrived: \
         {conversation:?}"
    );
}

/// `map` mounted as a window's whole root, laid out at `viewport`, so its presses and keys go
/// through the same dispatch a window's do.
fn mounted(map: Map) -> crate::headless::FrameHarness {
    let mut harness =
        crate::headless::FrameHarness::for_root(crate::State::new(), map, (800, 600), theme());
    harness.relayout();
    harness
}

/// A middle drag from `(from_x, from_y)` by `(dx, dy)` in four motions, the press first and the
/// release last, a frame between — the gesture a person makes with the wheel's button.
fn middle_drag(
    harness: &mut crate::headless::FrameHarness,
    (from_x, from_y): (f64, f64),
    (dx, dy): (f64, f64),
) {
    harness.send(&crate::synthetic_input::cursor_moved(from_x, from_y));
    harness.send(&crate::synthetic_input::mouse_input(
        ElementState::Pressed,
        MouseButton::Middle,
    ));
    for step in 1..=4 {
        let fraction = f64::from(step) / 4.0;
        harness.send(&crate::synthetic_input::cursor_moved(
            from_x + dx * fraction,
            from_y + dy * fraction,
        ));
    }
    harness.send(&crate::synthetic_input::mouse_input(
        ElementState::Released,
        MouseButton::Middle,
    ));
}

/// A pin's rectangle on the screen as the region laid it out: what the person sees move.
fn laid(harness: &crate::headless::FrameHarness, key: &str) -> Rect {
    harness
        .root::<Map>()
        .laid
        .borrow()
        .get(key)
        .copied()
        .unwrap_or_else(|| panic!("the pin {key:?} is laid out"))
}

#[test]
fn every_middle_drag_pans_the_pins_the_same_distance() {
    // The live run's failure: a middle drag over the canvas panned once, and identical drags after
    // it did nothing. The camera moved each time, but the region that lays the pins out held the
    // camera it was built with, so the pins never moved again.
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let mut harness = mounted(map_of(&[("one", 100.0)], viewport));
    let start = laid(&harness, "one");
    let mut seen = start;
    for round in 1..=5 {
        middle_drag(&mut harness, (700.0, 500.0), (-120.0, -90.0));
        harness.relayout();
        let now = laid(&harness, "one");
        assert!(
            (now.x0 - (seen.x0 - 120.0)).abs() <= 1.0 && (now.y0 - (seen.y0 - 90.0)).abs() <= 1.0,
            "round {round}: the drag moved the pin (-120, -90) as the first one did: \
             {seen:?} became {now:?}"
        );
        seen = now;
    }
}

#[test]
fn the_arrow_keys_pan_a_canvas_whose_press_put_the_keyboard_in_it() {
    // The arrows pan the canvas from the keyboard a press gave it: without the press the keys are
    // some other surface's, and pan nothing.
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let mut harness = mounted(map_of(&[("one", 100.0)], viewport));
    let before = laid(&harness, "one");
    harness.send(&crate::synthetic_input::named_key(
        winit::keyboard::NamedKey::ArrowRight,
    ));
    harness.relayout();
    assert_eq!(
        laid(&harness, "one"),
        before,
        "no press put the keyboard in the canvas: the arrow panned nothing"
    );
    middle_drag(&mut harness, (700.0, 500.0), (4.0, 4.0));
    harness.relayout();
    assert!(
        harness.focused().is_some(),
        "the press put the keyboard in the canvas"
    );
    let mut before = laid(&harness, "one");
    for round in 1..=3 {
        harness.send(&crate::synthetic_input::named_key(
            winit::keyboard::NamedKey::ArrowRight,
        ));
        harness.relayout();
        let now = laid(&harness, "one");
        assert!(
            now.x0 < before.x0 - 1.0,
            "round {round}: the right arrow panned the canvas right as the first one did: \
             {before:?} became {now:?}"
        );
        before = now;
    }
}

#[test]
fn ctrl_and_a_wheel_notch_zooms_the_pins() {
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let mut harness = mounted(map_of(&[("one", 100.0)], viewport));
    let before = laid(&harness, "one");
    let _ = harness.dispatch_to_root(&crate::synthetic_input::cursor_moved(400.0, 300.0));
    let ctrl = winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
    // Zooming in a notch at a time: the pin widens with each, so the third is wider than the
    // first, and the lens stays what the person set — no gesture's zoom is undone by the next.
    let mut seen = before.width();
    for round in 1..=3 {
        let _ =
            harness.dispatch_to_root_with(&crate::synthetic_input::mouse_wheel(0.0, 1.0), &ctrl);
        harness.relayout();
        let now = laid(&harness, "one").width();
        assert!(
            now > seen + 1.0,
            "round {round}: Ctrl and a notch zoomed the canvas in: {seen} became {now}"
        );
        seen = now;
    }
}

#[test]
fn places_opened_beside_a_conversation_never_shrink_it() {
    // The live run's failure: an agent opened Mail beside its conversation and the camera zoomed
    // everything into the viewport, so the conversation and every place were drawn as a narrow
    // column. A place that arrives beside another comes into view by the camera panning, never by
    // zooming, so every place keeps the size it was drawn at however many arrive. Here three
    // answers arrive one row at a time, down the canvas, as `free_place_next_to` puts them.
    let _frame = crate::motion::pin_frame();
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let mut map = map_of(&[("conversation", 0.0)], viewport);
    draw(&mut map);
    let camera = map.camera();
    let on_screen = |camera: MapCamera, (x, y): (f64, f64)| {
        Rect::from_origin_size(
            camera.to_screen(viewport.origin(), Point::new(x, y)),
            (200.0 * camera.zoom, 120.0 * camera.zoom),
        )
    };
    let conversation = on_screen(camera, (0.0, 0.0));
    assert!(
        conversation.y0 >= viewport.y0 && conversation.y1 <= viewport.y1,
        "the conversation starts in view: {conversation:?} in {viewport:?}"
    );
    // Three answers, each one row below the last of the two columns `free_place_next_to` walks.
    let mut held: Vec<(String, f64)> = vec![("conversation".to_owned(), 0.0)];
    for round in 1..=3 {
        held.push((format!("answer-{round}"), 544.0 * f64::from(round)));
        let pins: Vec<(&str, f64, f64)> = held
            .iter()
            .map(|(key, y)| (key.as_str(), 0.0, *y))
            .collect();
        let mut next = pins_at(&pins);
        drop(map);
        lay_out_again(&mut next, viewport);
        draw(&mut next);
        map = next;
        let camera = map.camera();
        // The zoom is the person's: no place arriving zooms it, so every place keeps its size.
        assert!(
            (camera.zoom - 1.0).abs() < f64::EPSILON,
            "round {round}: the camera kept the person's zoom, never shrinking the places: {}",
            camera.zoom
        );
        // What arrived is brought into view by the pan, whole.
        let arrived = on_screen(camera, (0.0, 544.0 * f64::from(round)));
        assert!(
            arrived.y0 >= viewport.y0 && arrived.y1 <= viewport.y1,
            "round {round}: answer {round} is in view: {arrived:?} in {viewport:?}"
        );
    }
}
