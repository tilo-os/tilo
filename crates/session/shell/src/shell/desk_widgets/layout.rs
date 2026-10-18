//! Where each desk widget waits: its edge, its place in the stack of
//! widgets sharing that edge, and around the system activities (a call, a
//! navigation card) already floating there.
//!
//! Widgets that share an edge stack along it, away from its corner or out
//! from its middle. A system activity floating over an edge is an occupant
//! the stack steps past, so a call card and a music widget never sit on
//! each other: system activities and widgets share the edges, and the
//! activity wins its place.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "placement is finite logical-pixel kurbo arithmetic bounded by the display"
)]

use vello::kurbo::{Point, Rect, Size, Vec2};

use super::{DockEdge, Slot, WidgetId};

/// Gap between a widget and the display edge, and between stacked widgets.
pub const MARGIN: f64 = 12.0;
/// How far around a glance form a pointer coming to rest still counts as
/// reaching for it; behind the form the zone runs on to the display edge.
/// Kept small: inside it, a resting pointer brings the widget, so any wider
/// and an app control near the edge would bring it when aimed at.
pub const REACH_PAD: f64 = 8.0;

/// One widget to place.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Item {
    pub id: WidgetId,
    pub edge: DockEdge,
    pub glance: Size,
    pub open: Size,
    pub has_content: bool,
}

/// Lay out `items` in `area` (the work area of the display the desk is on).
/// With `stack_open`, widgets stack by their open size (the Super surface,
/// where every card is open); otherwise by their glance size, and an open
/// card grows from its glance's corner over its neighbours instead of
/// pushing them.
#[must_use]
pub fn place(area: Rect, items: &[Item], occupied: &[Rect], stack_open: bool) -> Vec<Slot> {
    let mut taken: Vec<Rect> = occupied.to_vec();
    let mut out = Vec::with_capacity(items.len());
    for edge in DockEdge::ALL {
        let mut cursor = 0.0;
        for item in items.iter().filter(|item| item.edge == edge) {
            let stacked = if stack_open { item.open } else { item.glance };
            let mut rect = stacked_rect(area, edge, stacked, cursor);
            // Step past whatever already floats there.
            for _ in 0..=taken.len() {
                let Some(blocker) = taken.iter().find(|r| r.overlaps(rect)).copied() else {
                    break;
                };
                cursor += past(edge, rect, blocker);
                rect = stacked_rect(area, edge, stacked, cursor);
            }
            cursor += along(edge, stacked) + MARGIN;
            taken.push(rect);
            let glance = anchor_in(rect, edge, item.glance);
            let open = clamp_into(anchor_in(rect, edge, item.open), area);
            out.push(Slot {
                id: item.id,
                glance,
                open,
                reach: reach_zone(area, edge, glance),
                has_content: item.has_content,
            });
        }
    }
    out
}

/// The edge nearest to `p`, for a widget dropped there.
#[must_use]
pub fn nearest_edge(area: Rect, p: Point) -> DockEdge {
    let third_w = area.width() / 3.0;
    let low = p.y > area.y0 + area.height() * 0.75;
    let left = p.x < area.x0 + third_w;
    let right = p.x > area.x1 - third_w;
    match (low, left, right) {
        (true, true, _) => DockEdge::BottomLeft,
        (true, _, true) => DockEdge::BottomRight,
        (true, false, false) => DockEdge::Bottom,
        (false, _, _) if p.x < area.center().x => DockEdge::Left,
        (false, _, _) => DockEdge::Right,
    }
}

/// Length of `size` along the stacking direction of `edge`.
fn along(edge: DockEdge, size: Size) -> f64 {
    match edge {
        DockEdge::Left | DockEdge::Right | DockEdge::BottomLeft | DockEdge::BottomRight => {
            size.height
        }
        DockEdge::Bottom => size.width,
    }
}

/// How far along the stack `rect` must move to clear `blocker`.
fn past(edge: DockEdge, rect: Rect, blocker: Rect) -> f64 {
    let gap = MARGIN;
    match edge {
        // Corners stack upward.
        DockEdge::BottomLeft | DockEdge::BottomRight => (rect.y1 - blocker.y0) + gap,
        // Sides stack downward from the middle.
        DockEdge::Left | DockEdge::Right => (blocker.y1 - rect.y0) + gap,
        DockEdge::Bottom => (blocker.x1 - rect.x0) + gap,
    }
    .max(1.0)
}

/// The rect of a widget of `size` at `offset` along `edge`'s stack.
fn stacked_rect(area: Rect, edge: DockEdge, size: Size, offset: f64) -> Rect {
    let origin = match edge {
        DockEdge::BottomRight => Point::new(
            area.x1 - MARGIN - size.width,
            area.y1 - MARGIN - size.height - offset,
        ),
        DockEdge::BottomLeft => {
            Point::new(area.x0 + MARGIN, area.y1 - MARGIN - size.height - offset)
        }
        DockEdge::Right => Point::new(
            area.x1 - MARGIN - size.width,
            area.center().y - size.height / 2.0 + offset,
        ),
        DockEdge::Left => Point::new(
            area.x0 + MARGIN,
            area.center().y - size.height / 2.0 + offset,
        ),
        DockEdge::Bottom => Point::new(
            area.center().x - size.width / 2.0 + offset,
            area.y1 - MARGIN - size.height,
        ),
    };
    Rect::from_origin_size(origin, size)
}

/// `size` pinned into `slot` at the corner (or edge midpoint) nearest the
/// display edge, so the open card and its glance share their anchor.
fn anchor_in(slot: Rect, edge: DockEdge, size: Size) -> Rect {
    let origin = match edge {
        DockEdge::BottomRight => Point::new(slot.x1 - size.width, slot.y1 - size.height),
        DockEdge::BottomLeft => Point::new(slot.x0, slot.y1 - size.height),
        DockEdge::Right => Point::new(slot.x1 - size.width, slot.center().y - size.height / 2.0),
        DockEdge::Left => Point::new(slot.x0, slot.center().y - size.height / 2.0),
        DockEdge::Bottom => Point::new(slot.center().x - size.width / 2.0, slot.y1 - size.height),
    };
    Rect::from_origin_size(origin, size)
}

fn clamp_into(rect: Rect, area: Rect) -> Rect {
    let dx = if rect.x0 < area.x0 + MARGIN {
        area.x0 + MARGIN - rect.x0
    } else if rect.x1 > area.x1 - MARGIN {
        area.x1 - MARGIN - rect.x1
    } else {
        0.0
    };
    let dy = if rect.y0 < area.y0 + MARGIN {
        area.y0 + MARGIN - rect.y0
    } else if rect.y1 > area.y1 - MARGIN {
        area.y1 - MARGIN - rect.y1
    } else {
        0.0
    };
    rect + Vec2::new(dx, dy)
}

/// The glance form padded, extended to the display edge behind it.
fn reach_zone(area: Rect, edge: DockEdge, glance: Rect) -> Rect {
    let pad = glance.inflate(REACH_PAD, REACH_PAD);
    let zone = match edge {
        DockEdge::BottomRight => Rect::new(pad.x0, pad.y0, area.x1, area.y1),
        DockEdge::BottomLeft => Rect::new(area.x0, pad.y0, pad.x1, area.y1),
        DockEdge::Right => Rect::new(pad.x0, pad.y0, area.x1, pad.y1),
        DockEdge::Left => Rect::new(area.x0, pad.y0, pad.x1, pad.y1),
        DockEdge::Bottom => Rect::new(pad.x0, pad.y0, pad.x1, area.y1),
    };
    zone.intersect(area)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0.0, 24.0, 1600.0, 900.0);

    fn item(id: WidgetId, edge: DockEdge) -> Item {
        Item {
            id,
            edge,
            glance: Size::new(200.0, 36.0),
            open: Size::new(320.0, 140.0),
            has_content: true,
        }
    }

    #[test]
    fn widgets_wait_at_their_edges_and_never_at_the_top() {
        let slots = place(
            AREA,
            &[
                item(WidgetId::Music, DockEdge::BottomRight),
                item(WidgetId::Team, DockEdge::Right),
            ],
            &[],
            false,
        );
        let music = slots.iter().find(|s| s.id == WidgetId::Music).unwrap();
        let team = slots.iter().find(|s| s.id == WidgetId::Team).unwrap();
        assert_eq!(music.glance.x1, AREA.x1 - MARGIN);
        assert_eq!(music.glance.y1, AREA.y1 - MARGIN);
        assert_eq!(
            music.open.x1, music.glance.x1,
            "open grows from the same corner"
        );
        assert_eq!(music.open.y1, music.glance.y1);
        assert!((team.glance.center().y - AREA.center().y).abs() < 1.0);
        for slot in &slots {
            assert!(
                slot.glance.y0 > AREA.y0 + 100.0,
                "{slot:?} keeps off the top edge"
            );
            assert!(slot.reach.contains(slot.glance.center()));
            assert!(AREA.contains(slot.open.origin()));
        }
        assert_eq!(
            music.reach.x1, AREA.x1,
            "reach runs to the edge behind the form"
        );
        assert_eq!(music.reach.y1, AREA.y1);
    }

    #[test]
    fn widgets_sharing_an_edge_stack_and_step_past_a_floating_call() {
        let items = [
            item(WidgetId::Music, DockEdge::BottomRight),
            item(WidgetId::Team, DockEdge::BottomRight),
        ];
        let plain = place(AREA, &items, &[], false);
        assert!(plain[1].glance.y1 <= plain[0].glance.y0 - MARGIN + 0.5);
        let call = Rect::new(1300.0, 700.0, 1590.0, 890.0);
        let around = place(AREA, &items, &[call], false);
        for slot in &around {
            assert!(!slot.glance.overlaps(call), "{slot:?} under the call card");
        }
        assert!(around[0].glance.y1 <= call.y0);
    }

    #[test]
    fn a_drop_snaps_to_the_nearest_edge() {
        assert_eq!(
            nearest_edge(AREA, Point::new(1550.0, 880.0)),
            DockEdge::BottomRight
        );
        assert_eq!(
            nearest_edge(AREA, Point::new(40.0, 880.0)),
            DockEdge::BottomLeft
        );
        assert_eq!(
            nearest_edge(AREA, Point::new(800.0, 880.0)),
            DockEdge::Bottom
        );
        assert_eq!(
            nearest_edge(AREA, Point::new(1500.0, 300.0)),
            DockEdge::Right
        );
        assert_eq!(nearest_edge(AREA, Point::new(100.0, 60.0)), DockEdge::Left);
    }
}
