//! Reusable **drag-to-reorder** interaction model — the single source of
//! truth for live-reflow list reordering (weather's saved cities today;
//! notes, playlists, contacts next).
//!
//! Like [`Scrollbar`](crate::widget::scrollbar), this is a pure state
//! machine, not a widget: it turns `(list geometry, cursor)` into the live
//! insertion index, per-item reflow shift, and floating ghost rect.
//! [`DragController::cancel`] just forgets the session, so cancel is a no-op
//! by construction. A generic `Id` payload lets a caller key
//! selection/presence to the moving item without the controller
//! understanding it.
//!
//! The reflow math assumes a **uniform** list (one extent + fixed gap per
//! item) — true of every current adopter. A non-uniform list can still drive
//! the insertion index via its own [`ListMetrics`], but the whole-slot
//! reflow shift only makes sense for uniform slots.

use crate::num::f64_of_usize;
use vello::kurbo::{Point, Rect, Size};

/// Which way a reorderable list runs. The controller pins the *cross* axis
/// while dragging so the ghost slides only along the list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    Vertical,
    Horizontal,
}

/// Pixels of pointer travel before a press is promoted to a reorder drag, so
/// a plain click never jitters into a move.
pub const REORDER_THRESHOLD_PX: f64 = 5.0;

/// Uniform geometry of a reorderable list along the drag axis, in the same
/// (logical, scroll-adjusted) coordinate space as the cursor.
///
/// `origin` is the **leading edge** of item 0, *after* any scroll offset —
/// exactly where the first slot paints this frame. Rebuild it each frame
/// from live layout so the insertion index and reflow stay correct while
/// the list scrolls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListMetrics {
    pub origin: f64,
    pub item_extent: f64,
    pub gap: f64,
    pub count: usize,
}

impl ListMetrics {
    pub fn uniform(origin: f64, item_extent: f64, gap: f64, count: usize) -> Self {
        Self {
            origin,
            item_extent,
            gap,
            count,
        }
    }

    /// Distance from one slot's leading edge to the next (item + gap).
    pub fn pitch(&self) -> f64 {
        self.item_extent + self.gap
    }

    /// Leading-edge coordinate of slot boundary `i` (`0..=count`): boundary
    /// `0` sits before the first item, boundary `count` after the last.
    pub fn slot_lead(&self, i: usize) -> f64 {
        self.origin + f64_of_usize(i) * self.pitch()
    }

    /// The insertion **boundary** (`0..=count`) nearest `axis_coord` —
    /// snaps to the closest seam by rounding, so the gap flips at the
    /// midpoint of each slot.
    pub fn insertion_at(&self, axis_coord: f64) -> usize {
        if self.count == 0 || self.pitch() <= 0.0 {
            return 0;
        }
        let rel = (axis_coord - self.origin) / self.pitch();
        let boundary = rel.round().max(0.0);
        // `boundary` is non-negative by construction and the result is
        // immediately clamped to `self.count` below, so this cast cannot
        // misread a sign or lose a bit that matters — an out-of-range
        // `boundary` just saturates and is then capped by `.min`.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "boundary is non-negative and the result is immediately clamped to \
                      self.count below"
        )]
        let idx = boundary as usize;
        idx.min(self.count)
    }
}

/// The active (or pending) drag. Held inside [`DragController`]; not
/// constructed directly.
#[derive(Clone, Debug)]
struct Session<Id> {
    id: Id,
    /// Original index of the grabbed item in the pre-drag list.
    from: usize,
    /// Pointer offset within the grabbed item at grab time, so the ghost
    /// keeps the exact point picked up rather than snapping to the cursor.
    grab_offset: f64,
    /// Pointer position at press (for the drag-start threshold).
    start: Point,
    current: Point,
    /// Until the pointer crosses the drag-start threshold, there is no
    /// ghost, no gap, and a release is treated as a plain click.
    active: bool,
    /// Live target index, recomputed from the ghost's displacement as the
    /// pointer moves. Equals `from` while the ghost overlaps its own
    /// original slot (the dead-zone), so a small nudge opens no gap.
    target: usize,
}

/// The cursor's coordinate along `axis`.
fn axis_coord(axis: Axis, p: Point) -> f64 {
    match axis {
        Axis::Vertical => p.y,
        Axis::Horizontal => p.x,
    }
}

/// The target index the dragged item lands at, derived from how far its
/// ghost has travelled from its original slot. Rounding the slot-displacement
/// gives a half-slot dead-zone: the gap only moves once the ghost's leading
/// edge crosses the midpoint of a neighbouring slot.
fn target_for<Id>(axis: Axis, s: &Session<Id>, m: &ListMetrics) -> usize {
    if m.count == 0 {
        return 0;
    }
    let ghost_lead = axis_coord(axis, s.current) - s.grab_offset;
    let orig_lead = m.slot_lead(s.from);
    let delta = ((ghost_lead - orig_lead) / m.pitch()).round();
    let max_index = f64_of_usize(m.count.saturating_sub(1));
    let target = (f64_of_usize(s.from) + delta).clamp(0.0, max_index);
    // `target` is clamped into `[0.0, max_index]` immediately above (and
    // `max_index` itself came from a `usize` via the lossless helper), so
    // this cast is exact.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "target is clamped into [0.0, m.count - 1] immediately above"
    )]
    let idx = target as usize;
    idx
}

/// A single-axis drag-to-reorder controller. Construct one per reorderable
/// list ([`DragController::vertical`] / [`DragController::horizontal`]) and
/// drive it from the owner's event handler:
///
/// ```ignore
/// let grab = cursor.y - metrics.slot_lead(i);      // press on item i
/// ctrl.begin(item_id, i, grab, cursor);
/// if ctrl.pointer_moved(cursor, &metrics) { /* mark dirty */ }
/// for (i, item) in items { let dy = ctrl.item_shift(i, &metrics); }
/// if let Some(g) = ctrl.ghost_rect(list_x0, list_x1) { /* paint ghost */ }
/// if let Some((from, to)) = ctrl.commit() { model.move_item(from, to); }
/// ctrl.cancel(); // Escape / drop outside
/// ```
pub struct DragController<Id = ()> {
    axis: Axis,
    threshold: f64,
    session: Option<Session<Id>>,
}

impl<Id: Clone> DragController<Id> {
    fn new(axis: Axis) -> Self {
        Self {
            axis,
            threshold: REORDER_THRESHOLD_PX,
            session: None,
        }
    }

    /// A vertical reorderable list (items stack top-to-bottom, ghost slides
    /// along Y).
    pub fn vertical() -> Self {
        Self::new(Axis::Vertical)
    }

    /// A horizontal reorderable list (items run left-to-right, ghost slides
    /// along X).
    pub fn horizontal() -> Self {
        Self::new(Axis::Horizontal)
    }

    pub fn axis(&self) -> Axis {
        self.axis
    }

    /// The cursor's coordinate along this controller's axis.
    fn axis_of(&self, p: Point) -> f64 {
        match self.axis {
            Axis::Vertical => p.y,
            Axis::Horizontal => p.x,
        }
    }

    // --- lifecycle -----------------------------------------------------

    /// Begin a (pending) drag of the item at index `from`, carrying `id`.
    /// `grab_offset` is the pointer's offset *within* the grabbed item
    /// (`cursor_axis - item_leading_edge`). Not yet "active" — promotes to a
    /// live reorder only once the pointer clears the threshold.
    pub fn begin(&mut self, id: Id, from: usize, grab_offset: f64, start: Point) {
        self.session = Some(Session {
            id,
            from,
            grab_offset,
            start,
            current: start,
            active: false,
            target: from,
        });
    }

    /// Feed the latest pointer position. Promotes the drag to active once it
    /// clears the threshold. Returns `true` when the visible state changed
    /// (activation or a moved gap) so the owner can redraw only on an edge.
    pub fn pointer_moved(&mut self, cursor: Point, metrics: &ListMetrics) -> bool {
        let axis = self.axis;
        let threshold = self.threshold;
        let Some(s) = self.session.as_mut() else {
            return false;
        };
        s.current = cursor;
        let mut changed = false;
        if !s.active && s.start.distance(cursor) >= threshold {
            s.active = true;
            changed = true;
        }
        if s.active {
            let to = target_for(axis, s, metrics);
            if to != s.target {
                s.target = to;
                changed = true;
            }
        }
        changed
    }

    /// Recompute the live target index from the freshest `metrics` without a
    /// pointer move — call once per frame in `draw` so the gap stays pinned
    /// to the ghost while the list scrolls under a held drag.
    pub fn sync_insertion(&mut self, metrics: &ListMetrics) {
        let axis = self.axis;
        if let Some(s) = self.session.as_mut()
            && s.active
        {
            s.target = target_for(axis, s, metrics);
        }
    }

    /// End the drag, yielding `(from, to)` to apply to the model, or `None`
    /// when the drag never became active or the item didn't move. Clears
    /// the session either way. `to` is the final index after
    /// removal-and-reinsertion (`Vec::remove(from)` + `insert(to, _)`).
    pub fn commit(&mut self) -> Option<(usize, usize)> {
        let s = self.session.take()?;
        if !s.active {
            return None;
        }
        (s.target != s.from).then_some((s.from, s.target))
    }

    /// Abort the drag (Escape, or a drop outside the list). The model is
    /// only mutated on [`commit`](DragController::commit), so this simply
    /// forgets the session. Returns `true` if a drag was in progress.
    pub fn cancel(&mut self) -> bool {
        self.session.take().is_some()
    }

    // --- queries -------------------------------------------------------

    /// Whether a press has latched (active or not). Wire this to
    /// [`Widget::wants_pointer_capture`](crate::widget::Widget::wants_pointer_capture)
    /// so the drag keeps receiving events after the cursor leaves the item.
    pub fn is_pending(&self) -> bool {
        self.session.is_some()
    }

    /// Whether the drag has crossed the threshold and is live (ghost + gap
    /// showing).
    pub fn is_active(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.active)
    }

    /// The original index of the item being dragged, once the drag is active.
    pub fn from_index(&self) -> Option<usize> {
        self.session.as_ref().filter(|s| s.active).map(|s| s.from)
    }

    /// The live target index the dragged item would land at, once active —
    /// the `to` a drop would commit. Paint the insertion caret at this slot
    /// so the caret and the reflowed gap coincide.
    pub fn insertion(&self) -> Option<usize> {
        self.session.as_ref().filter(|s| s.active).map(|s| s.target)
    }

    /// The identity payload attached to the dragged item, once active.
    pub fn dragged_id(&self) -> Option<&Id> {
        self.session.as_ref().filter(|s| s.active).map(|s| &s.id)
    }

    // --- reflow + ghost ------------------------------------------------

    /// How far the *non-dragged* item at original index `index` must slide
    /// (signed) to open a one-slot gap at the live target index. `0` for the
    /// dragged item, and `0` for every item during the dead-zone. Models
    /// "remove the dragged item, then open a gap where it will land": an
    /// item collapses into the vacated slot, then shifts past the gap if
    /// the drop lands before it.
    pub fn item_shift(&self, index: usize, metrics: &ListMetrics) -> f64 {
        let Some(s) = self.session.as_ref() else {
            return 0.0;
        };
        if !s.active || index == s.from {
            return 0.0;
        }
        let to = s.target;
        // Index within the list with the dragged item removed. Only reached
        // when `index != s.from`, and this branch means `index > s.from`, so
        // `index >= 1` and the subtraction cannot underflow — `saturating_sub`
        // just makes that provable to the reader (and the lint) too.
        let collapsed = if index < s.from {
            index
        } else {
            index.saturating_sub(1)
        };
        // Index after re-inserting the dragged item at `to`.
        let final_i = if collapsed < to {
            collapsed
        } else {
            collapsed.saturating_add(1)
        };
        (f64_of_usize(final_i) - f64_of_usize(index)) * metrics.pitch()
    }

    /// The leading-edge coordinate of the floating ghost along the axis
    /// (`cursor_axis - grab_offset`), once active. Combine with the list's
    /// cross-axis bounds via [`ghost_rect`](DragController::ghost_rect).
    pub fn ghost_lead(&self) -> Option<f64> {
        self.session
            .as_ref()
            .filter(|s| s.active)
            .map(|s| self.axis_of(s.current) - s.grab_offset)
    }

    /// The floating ghost rect: one item-extent long along the axis, pinned
    /// to the list's cross-axis span `[cross_lo, cross_hi]`, positioned so the
    /// grab point stays under the cursor. `None` unless the drag is active.
    pub fn ghost_rect(&self, cross_lo: f64, cross_hi: f64, metrics: &ListMetrics) -> Option<Rect> {
        let lead = self.ghost_lead()?;
        Some(match self.axis {
            Axis::Vertical => Rect::new(cross_lo, lead, cross_hi, lead + metrics.item_extent),
            Axis::Horizontal => Rect::new(lead, cross_lo, lead + metrics.item_extent, cross_hi),
        })
    }

    /// Convenience ghost rect from an explicit item `size` (for callers whose
    /// item cross-extent differs from the list metrics), anchored so the grab
    /// point tracks the cursor. `None` unless active.
    pub fn ghost_rect_sized(&self, cross_lo: f64, size: Size) -> Option<Rect> {
        let lead = self.ghost_lead()?;
        Some(match self.axis {
            Axis::Vertical => Rect::new(cross_lo, lead, cross_lo + size.width, lead + size.height),
            Axis::Horizontal => {
                Rect::new(lead, cross_lo, lead + size.width, cross_lo + size.height)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4-item vertical list: 60-px items, 8-px gaps, first slot at y=100.
    /// Pitch = 68. Slot leads: 100, 168, 236, 304, boundary4 = 372.
    fn m4() -> ListMetrics {
        ListMetrics::uniform(100.0, 60.0, 8.0, 4)
    }

    #[test]
    fn insertion_snaps_to_seams_with_an_own_slot_dead_zone() {
        fn insertion_snaps_to_nearest_seam() {
            let m = m4();
            // Right at the top → before item 0.
            assert_eq!(m.insertion_at(100.0), 0);
            // Just past the middle of slot 0 (100..160, midpoint of pitch 68 = 134)
            // rounds up to seam 1.
            assert_eq!(m.insertion_at(140.0), 1);
            // Deep in slot 2 rounds to its trailing seam.
            assert_eq!(m.insertion_at(250.0), 2);
            // Far below the list clamps to the final boundary.
            assert_eq!(m.insertion_at(10_000.0), 4);
            // Above the list clamps to 0.
            assert_eq!(m.insertion_at(-500.0), 0);
        }

        fn nudge_within_own_slot_is_a_dead_zone_no_gap_no_reflow() {
            let m = m4();
            let mut c = DragController::<u32>::vertical();
            // Grab item 1 at offset 30 (ghost lead = slot_lead(1)).
            c.begin(1, 1, 30.0, Point::new(200.0, 198.0));
            // A nudge that clears the drag-start threshold but keeps the ghost
            // inside item 1's own slot must NOT open a gap.
            c.pointer_moved(Point::new(200.0, 205.0), &m);
            assert!(c.is_active(), "the nudge cleared the threshold");
            assert_eq!(
                c.insertion(),
                Some(1),
                "the ghost is still over its own slot → target stays `from`, no gap opens"
            );
            for i in 0..4 {
                assert_eq!(
                    c.item_shift(i, &m),
                    0.0,
                    "no item reflows while the ghost is in its own slot's dead-zone (i={i})"
                );
            }
            assert_eq!(
                c.commit(),
                None,
                "dropping where it started changes nothing"
            );
        }

        fn gap_opens_only_once_the_ghost_crosses_into_a_neighbour() {
            let m = m4(); // pitch 68, slot 1 spans lead 168..236
            let mut c = DragController::<u32>::vertical();
            c.begin(1, 1, 30.0, Point::new(200.0, 198.0)); // grab item 1 at its middle
            // Just past the neighbour's midpoint downward: ghost lead moves ~0.6
            // slot, which rounds to a one-slot move → the gap finally opens.
            c.pointer_moved(Point::new(200.0, 198.0 + 0.6 * m.pitch()), &m);
            assert_eq!(
                c.insertion(),
                Some(2),
                "crossing the midpoint opens the gap below"
            );
            assert!(
                c.item_shift(2, &m) != 0.0,
                "the neighbour reflows once the gap has moved onto it"
            );
        }

        fn insertion_equals_the_committed_destination() {
            // The live target index the caret/gap is drawn at must equal the `to`
            // a drop commits, so what you see is exactly what lands.
            let mut c = DragController::<u32>::vertical();
            let m = m4();
            c.begin(0, 0, 30.0, Point::new(200.0, 130.0));
            c.pointer_moved(Point::new(200.0, 400.0), &m); // drag item 0 to the end
            let shown = c.insertion();
            assert_eq!(shown, Some(3), "the gap sits at the final slot");
            assert_eq!(
                c.commit().map(|(_, to)| to),
                shown,
                "drop lands where the gap showed"
            );
        }

        insertion_snaps_to_nearest_seam();
        nudge_within_own_slot_is_a_dead_zone_no_gap_no_reflow();
        gap_opens_only_once_the_ghost_crosses_into_a_neighbour();
        insertion_equals_the_committed_destination();
    }

    #[test]
    fn drag_session_press_move_drop_and_cancel() {
        fn press_then_small_move_stays_a_click() {
            let mut c = DragController::<u32>::vertical();
            c.begin(7, 1, 30.0, Point::new(200.0, 200.0));
            assert!(c.is_pending(), "press latched");
            assert!(!c.is_active(), "not yet a drag");
            // A jitter under the threshold does not activate.
            let changed = c.pointer_moved(Point::new(201.0, 202.0), &m4());
            assert!(!changed);
            assert!(!c.is_active());
            // Release without activating commits nothing (it was a click).
            assert_eq!(c.commit(), None);
        }

        fn pointer_move_updates_live_insertion_index() {
            let mut c = DragController::<u32>::vertical();
            // Grab item 0 at its centre (offset 30 within a 60-px item).
            c.begin(0, 0, 30.0, Point::new(200.0, 130.0));
            // Drag down into slot 2 → live target tracks toward the bottom.
            assert!(c.pointer_moved(Point::new(200.0, 250.0), &m4()));
            assert!(c.is_active(), "cleared the threshold");
            assert_eq!(c.insertion(), Some(2));
            // Keep dragging past the last item → target follows to the last slot.
            c.pointer_moved(Point::new(200.0, 400.0), &m4());
            assert_eq!(
                c.insertion(),
                Some(3),
                "gap tracks the ghost live, not on release"
            );
        }

        fn drop_commits_from_to_move() {
            let mut c = DragController::<u32>::vertical();
            c.begin(0, 0, 30.0, Point::new(200.0, 130.0));
            c.pointer_moved(Point::new(200.0, 400.0), &m4()); // drag item 0 to the end
            // insertion boundary 4, from 0 → final index 3.
            assert_eq!(c.commit(), Some((0, 3)));
            assert!(!c.is_pending(), "commit clears the session");
        }

        fn cancel_restores_by_forgetting_the_session() {
            let mut c = DragController::<u32>::vertical();
            c.begin(2, 2, 30.0, Point::new(200.0, 266.0));
            c.pointer_moved(Point::new(200.0, 120.0), &m4());
            assert!(c.is_active());
            assert!(c.cancel(), "a live drag was cancelled");
            assert!(!c.is_pending());
            // A subsequent commit yields nothing — the order is untouched.
            assert_eq!(c.commit(), None);
        }

        fn ghost_tracks_cursor_at_grab_offset() {
            let mut c = DragController::<u32>::vertical();
            let m = m4();
            // Grab item 1 (lead 168) at offset 20 from its top → grab point y=188.
            c.begin(1, 1, 20.0, Point::new(200.0, 188.0));
            c.pointer_moved(Point::new(200.0, 300.0), &m);
            // Ghost leading edge keeps the 20-px grab offset under the cursor.
            assert_eq!(c.ghost_lead(), Some(280.0));
            let g = c.ghost_rect(180.0, 520.0, &m).unwrap();
            assert_eq!(g.y0, 280.0);
            assert_eq!(g.height(), 60.0, "ghost is one item-extent tall");
            assert_eq!(g.x0, 180.0);
            assert_eq!(g.x1, 520.0);
        }

        fn horizontal_axis_uses_x() {
            let mut c = DragController::<u32>::horizontal();
            let m = ListMetrics::uniform(0.0, 80.0, 10.0, 3); // pitch 90
            c.begin(0, 0, 40.0, Point::new(40.0, 50.0)); // ghost lead = slot_lead(0)
            c.pointer_moved(Point::new(200.0, 50.0), &m); // slide right ~1.8 slots
            assert!(c.is_active());
            assert_eq!(
                c.insertion(),
                Some(2),
                "the ghost landed over the last slot"
            );
            assert_eq!(c.ghost_lead(), Some(160.0), "ghost tracks X at grab offset");
            assert_eq!(c.commit(), Some((0, 2)));
        }

        press_then_small_move_stays_a_click();
        pointer_move_updates_live_insertion_index();
        drop_commits_from_to_move();
        cancel_restores_by_forgetting_the_session();
        ghost_tracks_cursor_at_grab_offset();
        horizontal_axis_uses_x();
    }

    #[test]
    fn reflow_opens_and_repins_the_drop_gap() {
        fn reflow_opens_a_gap_at_the_drop_and_closes_the_hole() {
            // Drag item 0 to the end (insertion 4 → final index 3).
            let mut c = DragController::<u32>::vertical();
            let m = m4();
            c.begin(0, 0, 30.0, Point::new(200.0, 130.0));
            c.pointer_moved(Point::new(200.0, 400.0), &m);
            let pitch = m.pitch();
            // The dragged item does not participate in the reflow.
            assert_eq!(c.item_shift(0, &m), 0.0);
            // Every item below it slides up one slot to fill the vacated hole and
            // sit above the gap that opens at the tail.
            assert_eq!(c.item_shift(1, &m), -pitch);
            assert_eq!(c.item_shift(2, &m), -pitch);
            assert_eq!(c.item_shift(3, &m), -pitch);
        }

        fn reflow_pushes_items_down_when_dropping_above_them() {
            // Drag the last item (3) to the very top (insertion 0 → final 0).
            let mut c = DragController::<u32>::vertical();
            let m = m4();
            c.begin(9, 3, 30.0, Point::new(200.0, 334.0));
            c.pointer_moved(Point::new(200.0, 100.0), &m);
            assert_eq!(c.insertion(), Some(0));
            let pitch = m.pitch();
            // Items 0..=2 each slide down one slot to open the gap at the top.
            assert_eq!(c.item_shift(0, &m), pitch);
            assert_eq!(c.item_shift(1, &m), pitch);
            assert_eq!(c.item_shift(2, &m), pitch);
            // The dragged item itself is the ghost.
            assert_eq!(c.item_shift(3, &m), 0.0);
            assert_eq!(c.commit(), Some((3, 0)));
        }

        fn sync_insertion_repins_gap_after_a_scroll() {
            let mut c = DragController::<u32>::vertical();
            c.begin(0, 0, 30.0, Point::new(200.0, 130.0));
            c.pointer_moved(Point::new(200.0, 250.0), &m4());
            assert_eq!(c.insertion(), Some(2));
            // The list scrolls up 68px under the held drag; item 0's own slot moves
            // up with it, so the same ghost now points one slot lower.
            let scrolled = ListMetrics::uniform(32.0, 60.0, 8.0, 4);
            c.sync_insertion(&scrolled);
            assert_eq!(
                c.insertion(),
                Some(3),
                "gap re-pins to the ghost after scroll"
            );
        }

        reflow_opens_a_gap_at_the_drop_and_closes_the_hole();
        reflow_pushes_items_down_when_dropping_above_them();
        sync_insertion_repins_gap_after_a_scroll();
    }
}
