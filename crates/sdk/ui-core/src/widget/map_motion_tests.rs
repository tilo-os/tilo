//! A map's pins on their way (`motion::Placed`): a pin the person moves glides from where it
//! stood to its new place over the frames between, reaches it, and then rests asking for no
//! frames; a still theme and reduced motion land the pin at once.

use super::*;
use crate::Theme;

fn theme() -> Theme {
    Theme::dark()
}

/// A map of pins, laid out in a viewport.
fn map_of(pins: &[(&str, f64)], viewport: Rect) -> Map {
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
    let mut text = crate::font_catalog::fixture_renderer();
    crate::widget::layout::lay_out_detached(&mut map, viewport, &theme(), &mut text)
        .expect("the map lays out");
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
    let mut map = map_of(&[("one", 100.0)], viewport);
    map.carry_from(&old);
    assert!(
        map.camera().same(old.camera()),
        "the lens the person left the canvas at"
    );
    lay_out_again(&mut map, viewport);
    draw(&mut map);
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
    let mut map = map_of(&[("one", 0.0), ("two", 300.0)], viewport);
    draw(&mut map);
    map.carry_from(&old);
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
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let (mut old, mut map) = {
        let _frame = crate::motion::pin_frame_at(start);
        let mut old = map_of(&[("one", 0.0)], viewport);
        draw(&mut old);
        (old, map_of(&[("one", 0.0), ("two", 700.0)], viewport))
    };
    // The person opened it: the camera glides there, whoever's input came last.
    crate::motion::note_source(crate::tokens::MotionSource::Pointer);
    {
        let _frame = crate::motion::pin_frame_at(start);
        draw(&mut old);
        map.carry_from(&old);
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
            .all(|(one, two)| (two - one - 700.0).abs() < 0.5),
        "every place is laid out where the camera has carried it, each frame: {along:?}"
    );
    let (_, last) = *along.last().expect("the camera moved");
    assert!(
        (last - (800.0 - 200.0 - ARRIVAL_MARGIN)).abs() < 0.5,
        "it arrives whole in view: {along:?}"
    );
    assert!(
        along
            .iter()
            .any(|(_, two)| *two < 699.0 && *two > last + 1.0),
        "on its way over the frames between: {along:?}"
    );
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
        (Some(0.0), Some(700.0)),
        "no place on the canvas moved: only the lens did"
    );
}
