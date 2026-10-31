//! Where a desk widget rests, and how far an open card reaches from it.
//!
//! A widget rests wherever the person put it, on whichever display: its place
//! is kept per display as fractions of the room its resting form can move in
//! there ([`ui::desk_widgets::Place`]), so a widget against an edge or in a
//! corner stays there when the resolution changes.
//!
//! A widget's open card grows from its resting form toward the middle of the
//! display ([`open_rect`]): from the corner of the form nearest the display's
//! nearer sides, so opening never pushes it off the screen and the form and
//! the card share that corner.
//!
//! Everything here is pure geometry in desktop coordinates, tested on its
//! own; the desk feeds it displays and widgets.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "placement is finite logical-pixel kurbo arithmetic bounded by the display"
)]

use vello::kurbo::{Point, Rect, Size, Vec2};

/// Gap between a widget and the display edge, and between neighbours drawn
/// onto each other.
pub const MARGIN: f64 = ui::tokens::Spacing::BASE.s3 as f64;
/// How far around a resting form a pointer coming to rest still counts as
/// reaching for it; against a display edge the zone runs on to that edge.
pub const REACH_PAD: f64 = ui::tokens::Spacing::BASE.s2 as f64;

fn room(area: Rect, size: Size) -> Rect {
    let x0 = area.x0 + MARGIN;
    let y0 = area.y0 + MARGIN;
    let x1 = (area.x1 - MARGIN - size.width).max(x0);
    let y1 = (area.y1 - MARGIN - size.height).max(y0);
    Rect::new(x0, y0, x1, y1)
}

/// `size` at fractions `(x, y)` of its room in `area`.
#[must_use]
pub fn rect_at(area: Rect, size: Size, (x, y): (f64, f64)) -> Rect {
    let room = room(area, size);
    let origin = Point::new(
        room.x0 + room.width() * x.clamp(0.0, 1.0),
        room.y0 + room.height() * y.clamp(0.0, 1.0),
    );
    Rect::from_origin_size(origin, size)
}

/// The fractions `rect` stands at in its room in `area`: the inverse of
/// [`rect_at`]. A rect outside the room reads as the nearest edge.
#[must_use]
pub fn fractions(area: Rect, rect: Rect) -> (f64, f64) {
    let room = room(area, rect.size());
    let along = |at: f64, from: f64, span: f64| {
        if span <= f64::EPSILON {
            0.5
        } else {
            ((at - from) / span).clamp(0.0, 1.0)
        }
    };
    (
        along(rect.x0, room.x0, room.width()),
        along(rect.y0, room.y0, room.height()),
    )
}

/// `rect` moved the least distance that puts it inside `area`'s margins.
#[must_use]
pub fn clamp(area: Rect, rect: Rect) -> Rect {
    let room = room(area, rect.size());
    let origin = Point::new(
        rect.x0.clamp(room.x0, room.x1),
        rect.y0.clamp(room.y0, room.y1),
    );
    Rect::from_origin_size(origin, rect.size())
}

/// Which way an open card grows from the resting form at fractions
/// `(x, y)`: toward the display's middle. `(right, bottom)`: whether the
/// form and the card share their right edge, and their bottom edge.
#[must_use]
pub fn anchor(fractions: (f64, f64)) -> (bool, bool) {
    (fractions.0 >= 0.5, fractions.1 >= 0.5)
}

/// The open card of `size` for the resting form at `glance` in `area`:
/// sharing the form's corner nearest the display's nearer sides, then kept
/// on the display.
#[must_use]
pub fn open_rect(area: Rect, glance: Rect, size: Size) -> Rect {
    let (right, bottom) = anchor(fractions(area, glance));
    let x0 = if right {
        glance.x1 - size.width
    } else {
        glance.x0
    };
    let y0 = if bottom {
        glance.y1 - size.height
    } else {
        glance.y0
    };
    clamp(area, Rect::from_origin_size(Point::new(x0, y0), size))
}

/// The resting form of `size` whose open card is `open` in `area`: the
/// inverse of [`open_rect`], for a widget dragged and dropped open. Of the
/// corners it could share with the card, the one whose own place grows the
/// card back to `open`, preferring the card's own side of the display.
#[must_use]
pub fn glance_in(area: Rect, open: Rect, size: Size) -> Rect {
    let corner = |(right, bottom): (bool, bool)| {
        let x0 = if right { open.x1 - size.width } else { open.x0 };
        let y0 = if bottom {
            open.y1 - size.height
        } else {
            open.y0
        };
        Rect::from_origin_size(Point::new(x0, y0), size)
    };
    let (right, bottom) = anchor(fractions(area, open));
    [
        (right, bottom),
        (!right, bottom),
        (right, !bottom),
        (!right, !bottom),
    ]
    .into_iter()
    .map(corner)
    .find(|glance| {
        let back = open_rect(area, *glance, open.size());
        (back.origin() - open.origin()).hypot() < 0.5
    })
    .unwrap_or_else(|| corner((right, bottom)))
}

/// Where a pointer coming to rest reaches for the form at `glance`: the
/// form padded, run on to each display edge it rests against, so throwing
/// the pointer into its corner finds it.
#[must_use]
pub fn reach_zone(area: Rect, glance: Rect) -> Rect {
    let pad = glance.inflate(REACH_PAD, REACH_PAD);
    let near = MARGIN + REACH_PAD;
    Rect::new(
        if glance.x0 - area.x0 <= near {
            area.x0
        } else {
            pad.x0
        },
        if glance.y0 - area.y0 <= near {
            area.y0
        } else {
            pad.y0
        },
        if area.x1 - glance.x1 <= near {
            area.x1
        } else {
            pad.x1
        },
        if area.y1 - glance.y1 <= near {
            area.y1
        } else {
            pad.y1
        },
    )
    .intersect(area)
}

/// Lay `rects` (in their order) out of each other's way and out of
/// `occupied` (a live call card, the strip switcher), within `area`: each
/// one that overlaps something placed before it steps along the edge it
/// rests nearest, toward the display's middle, until it is clear. What is
/// stored never changes; this is only where each is drawn now.
pub fn resolve(area: Rect, rects: &mut [Rect], occupied: &[Rect]) {
    let mut taken: Vec<Rect> = occupied.to_vec();
    for rect in rects.iter_mut() {
        let (fx, fy) = fractions(area, *rect);
        // Along a side edge it steps up or down; along the bottom or top,
        // sideways.
        let vertical = (fx - 0.5).abs() >= (fy - 0.5).abs();
        let forward = if vertical { fy < 0.5 } else { fx < 0.5 };
        let mut step_dir = forward;
        let mut tries = 0;
        let start = *rect;
        while let Some(blocker) = taken.iter().find(|t| t.overlaps(*rect)).copied() {
            tries += 1;
            if tries > 2 * taken.len() + 2 {
                // Nowhere along this way: try the other, and past that
                // stay put rather than loop.
                if step_dir == forward {
                    step_dir = !forward;
                    *rect = start;
                    tries = 0;
                    continue;
                }
                *rect = start;
                break;
            }
            let shift = match (vertical, step_dir) {
                (true, true) => Vec2::new(0.0, blocker.y1 + MARGIN - rect.y0),
                (true, false) => Vec2::new(0.0, blocker.y0 - MARGIN - rect.y1),
                (false, true) => Vec2::new(blocker.x1 + MARGIN - rect.x0, 0.0),
                (false, false) => Vec2::new(blocker.x0 - MARGIN - rect.x1, 0.0),
            };
            let moved = *rect + shift;
            if clamp(area, moved) != moved {
                // Off the display this way.
                tries = usize::MAX / 2;
                continue;
            }
            *rect = moved;
        }
        taken.push(*rect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0.0, 32.0, 1600.0, 900.0);
    const PILL: Size = Size::new(240.0, 40.0);

    #[test]
    fn a_place_round_trips_and_keeps_to_its_edge_on_another_resolution() {
        for at in [(0.0, 0.0), (1.0, 1.0), (0.3, 0.7), (1.0, 0.5)] {
            let rect = rect_at(AREA, PILL, at);
            let back = fractions(AREA, rect);
            assert!((back.0 - at.0).abs() < 1e-9 && (back.1 - at.1).abs() < 1e-9);
        }
        let corner = rect_at(AREA, PILL, (1.0, 1.0));
        assert_eq!(corner.x1, AREA.x1 - MARGIN);
        assert_eq!(corner.y1, AREA.y1 - MARGIN);
        let bigger = Rect::new(0.0, 32.0, 2560.0, 1440.0);
        let there = rect_at(bigger, PILL, fractions(AREA, corner));
        assert_eq!(
            (there.x1, there.y1),
            (bigger.x1 - MARGIN, bigger.y1 - MARGIN)
        );
    }

    #[test]
    fn an_open_card_grows_toward_the_middle_and_back() {
        let open = Size::new(320.0, 200.0);
        for at in [(1.0, 1.0), (0.0, 1.0), (0.0, 0.0), (1.0, 0.0), (0.7, 0.2)] {
            let glance = rect_at(AREA, PILL, at);
            let card = open_rect(AREA, glance, open);
            assert!(AREA.contains(card.origin()) && AREA.contains(Point::new(card.x1, card.y1)));
            let (right, bottom) = anchor(at);
            if right {
                assert_eq!(card.x1, glance.x1, "{at:?}");
            } else {
                assert_eq!(card.x0, glance.x0, "{at:?}");
            }
            if bottom {
                assert_eq!(card.y1, glance.y1, "{at:?}");
            } else {
                assert_eq!(card.y0, glance.y0, "{at:?}");
            }
            let back = glance_in(AREA, card, PILL);
            assert!(
                (back.origin() - glance.origin()).hypot() < 0.5 && back.size() == PILL,
                "{at:?}: the card folds to where it grew from: {back:?}"
            );
        }
    }

    #[test]
    fn widgets_resting_together_step_apart_and_past_a_call() {
        let mut rects = vec![
            rect_at(AREA, PILL, (1.0, 1.0)),
            rect_at(AREA, PILL, (1.0, 1.0)),
        ];
        let call = Rect::new(1300.0, 700.0, 1590.0, 890.0);
        resolve(AREA, &mut rects, &[call]);
        for (k, rect) in rects.iter().enumerate() {
            assert!(!rect.overlaps(call), "{k} {rect:?}");
        }
        assert!(!rects[0].overlaps(rects[1]), "{rects:?}");
        for rect in &rects {
            assert_eq!(clamp(AREA, *rect), *rect, "kept on the display");
        }
    }

    #[test]
    fn the_reach_zone_runs_to_the_edges_it_rests_against() {
        let corner = rect_at(AREA, PILL, (1.0, 1.0));
        let zone = reach_zone(AREA, corner);
        assert_eq!((zone.x1, zone.y1), (AREA.x1, AREA.y1));
        let free = Rect::from_origin_size((600.0, 400.0), PILL);
        assert_eq!(reach_zone(AREA, free), free.inflate(REACH_PAD, REACH_PAD));
    }
}
