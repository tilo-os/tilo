//! Where the people overlay stands: at an edge, as a desk widget does.
//!
//! The overlay is one more thing kept at the edges of the screen, so it is
//! laid out, dragged and snapped by the desk's own edge mechanism
//! ([`crate::desk_edges`]): it waits at a [`DockEdge`], steps past the
//! widgets and system activities already there, follows the pointer while
//! dragged and, let go, snaps to the edge nearest the pointer. The host
//! keeps the edge where the desk widgets keep theirs
//! (`settings_store::desk_widgets::PEOPLE_OVERLAY`).
//!
//! Only the presentation is its own: from where it was let go, the panel
//! springs to its new slot instead of jumping there.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "placement is finite logical-pixel kurbo arithmetic bounded by the display"
)]

use vello::kurbo::{Point, Rect, Size, Vec2};

use ui::motion::{Direction, MotionValue, Snap};
use ui::time::Instant;
use ui::tokens::{EffectiveMotion, MotionSource};

use crate::desk_edges::{self, DockEdge, EdgeDrag, Item};

/// The overlay's placement: its edge, where it is drawn now, and a drag in
/// progress.
#[derive(Debug)]
pub struct Dock {
    edge: DockEdge,
    /// The slot at its edge, as last laid out.
    rest: Option<Rect>,
    /// How far from its slot the panel is drawn while it springs there
    /// after a drop.
    settle: MotionValue<Point>,
    /// Where the panel was drawn when let go, until the next layout starts
    /// the spring from there.
    dropped: Option<Point>,
    press: Option<EdgeDrag<()>>,
    /// The pointer during a press, in the host's coordinates.
    pointer: Point,
    area: Rect,
}

impl Dock {
    #[must_use]
    pub fn new(edge: DockEdge) -> Self {
        Self {
            edge,
            rest: None,
            settle: MotionValue::new(Point::ZERO),
            dropped: None,
            press: None,
            pointer: Point::ZERO,
            area: Rect::ZERO,
        }
    }

    #[must_use]
    pub fn edge(&self) -> DockEdge {
        self.edge
    }

    /// Wait at `edge` from now on (as the person's settings keep it),
    /// without a motion.
    pub fn set_edge(&mut self, edge: DockEdge) {
        self.edge = edge;
    }

    /// Lay the panel of `size` out at its edge in `area`, past `occupied`
    /// (widgets and activities already at the edges), and return where to
    /// draw it: in its slot, shifted by a drag, or springing from a drop.
    pub fn frame(
        &mut self,
        area: Rect,
        size: Size,
        occupied: &[Rect],
        motion: EffectiveMotion,
        now: Instant,
    ) -> Rect {
        self.area = area;
        let item = Item {
            id: (),
            edge: self.edge,
            glance: size,
            open: size,
            has_content: true,
        };
        let rest = desk_edges::place(area, &[item], occupied, false)
            .first()
            .map_or_else(
                || Rect::from_origin_size(area.origin(), size),
                |slot| slot.glance,
            );
        self.rest = Some(rest);
        if let Some(shift) = self.press.and_then(|press| press.shift(self.pointer)) {
            return rest + shift;
        }
        if let Some(dropped) = self.dropped.take() {
            let from = dropped - rest.origin();
            self.settle.snap(Point::new(from.x, from.y));
            let timing = Snap::SPEC.timing(Direction::Enter, motion, MotionSource::Gesture);
            self.settle.set(Point::ZERO, timing, now);
        }
        let offset = self.settle.get(now);
        rest + Vec2::new(offset.x, offset.y)
    }

    /// Whether the panel is springing to its slot.
    #[must_use]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.dropped.is_some() || self.settle.is_moving(now)
    }

    #[must_use]
    pub fn settles_at(&self) -> Option<Instant> {
        self.settle.settles_at()
    }

    /// A press at `point` on the panel: it may become a drag.
    pub fn press(&mut self, point: Point) {
        self.pointer = point;
        self.press = Some(EdgeDrag::new((), point));
    }

    /// The pointer moved to `point` while pressed. Returns whether the
    /// panel is being dragged (then it is drawn under the pointer).
    pub fn drag(&mut self, point: Point) -> bool {
        let Some(press) = self.press.as_mut() else {
            return false;
        };
        self.pointer = point;
        press.moved(point)
    }

    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.press.is_some_and(|press| press.dragging)
    }

    /// Whether a press is held on the panel (dragged or not yet).
    #[must_use]
    pub fn is_held(&self) -> bool {
        self.press.is_some()
    }

    /// The press ended at `point`. A drag snaps to the edge nearest the
    /// pointer and returns it (for the host to keep); a press that never
    /// dragged (a click) returns `None`.
    pub fn release(&mut self, point: Point) -> Option<DockEdge> {
        let press = self.press.take()?;
        let shift = press.shift(point);
        let edge = press.release(self.area, point)?;
        if let (Some(rest), Some(shift)) = (self.rest, shift) {
            self.dropped = Some(rest.origin() + shift);
        }
        self.edge = edge;
        Some(edge)
    }

    /// Drop a press (the pointer left the window): the panel springs back
    /// to its slot.
    pub fn cancel(&mut self) {
        if let Some(press) = self.press.take()
            && let (Some(rest), Some(shift)) = (self.rest, press.shift(self.pointer))
        {
            self.dropped = Some(rest.origin() + shift);
        }
    }
}
