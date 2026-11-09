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
    assert!(map.placed.get("one").is_some_and(|p| !p.is_moving(now)));
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
    assert!(map.placed.contains_key("one"), "the pin is drawn");

    // The person moves the pin: the map lays it out at its new point and draws again.
    crate::motion::note_source(crate::tokens::MotionSource::Pointer);
    map.pins[0].1.x = 400.0;
    map.invalidation.invalidate();
    lay_out_again(&mut map, viewport);
    draw(&mut map);
    assert!(
        map.placed
            .get("one")
            .is_some_and(|p| p.movement().is_moving(now)),
        "the pin's move began: {:?}",
        map.placed.get("one").map(|p| p.movement().target())
    );
    let start = map
        .placed
        .get("one")
        .map(|p| p.movement().sampled_after(crate::time::Duration::ZERO).0);
    let mid = map.placed.get("one").map(|p| {
        p.movement()
            .sampled_after(crate::time::Duration::from_millis(80))
            .0
    });
    let arrived = map.placed.get("one").map(|p| {
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
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let still = crate::tokens::EffectiveMotion::new(
        crate::tokens::Motion {
            still: true,
            ..crate::tokens::Motion::default()
        },
        false,
    );
    let mut map = map_of(&[("one", 100.0)], viewport);
    let now = crate::motion::frame_now();
    // Show the pin at its place under a still theme: it lands at once and asks for nothing.
    map.placed
        .insert("one".to_owned(), crate::motion::Placed::closed());
    let placed = map.placed.get_mut("one").expect("the pin");
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
