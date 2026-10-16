//! Pointer intent for a row of items that open on hover to show more
//! controls (a tab pill's search/maximize chips beside its ×).
//!
//! Opening an item changes the row's geometry: the item grows rightward
//! from its leading edge, so its own controls and everything after it move.
//! That makes three rules, in priority order:
//!
//! 1. **Pointing at a control is intent to click it.** A pointer resting on
//!    a control that is clickable now (a ×, a revealed chip, "new tab"), or
//!    on the spot where the item's controls will sit once open, never opens
//!    anything. Nothing ever moves a control the pointer is on: an open item
//!    whose collapse would shift the control under the pointer stays open
//!    until the pointer is off it.
//! 2. **Heading for a control is the same intent, early.** Each motion
//!    sample is checked against the triangle (the convex hull) spanned by a
//!    recent pointer position and a control. While the pointer travels inside
//!    the hull toward a control that a change would move, that change waits:
//!    an item the pointer crosses on its way to its own × does not open under
//!    it, and an open item does not collapse (sliding its neighbours) while
//!    the pointer heads for a neighbour's ×. This is the mega-menu "menu
//!    aim" prediction, applied to what would move rather than to a timer
//!    long enough for the slowest hand.
//! 3. **Seeing more needs a clear signal**: the pointer resting on an item's
//!    body, away from its controls, for a short [`DWELL`] counted from when it
//!    entered (a sweep across the row spends less than that on each item, so
//!    none opens). Once open, the item stays open while the pointer is in it
//!    or heading for its revealed controls (menu aim again), and closes the
//!    moment the pointer moves anywhere else.
//!
//! A held change resumes [`AIM_HOLD`] after the last sample that confirmed
//! the heading, so a pointer that stops short of where it was heading lets
//! go instead of pinning the row.
//!
//! The state is pure: every method takes the instant it reasons about, so
//! slow and fast pointer paths are tested against a synthetic timeline.

use crate::time::{Duration, Instant};
use vello::kurbo::{Point, Rect};

/// How long the pointer rests on an item's body before it opens. Shorter
/// than a deliberate pause, longer than the time a pointer sweeping across a
/// row spends over any one item.
pub(crate) const DWELL: Duration = Duration::from_millis(100);

/// How long a change held by menu aim waits once the pointer stops moving
/// toward what it was heading for without arriving. Also how long an item
/// waits to open when the last sample over it was heading for a control.
pub(crate) const AIM_HOLD: Duration = Duration::from_millis(300);

/// Recent pointer positions kept; the oldest is the triangle's apex, so a
/// single jittery sample cannot point the cone the wrong way.
const APEX_SAMPLES: usize = 3;

/// One item of the row, as laid out now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Item {
    /// The host's key for the item.
    pub(crate) idx: usize,
    /// Where it starts. Items grow and shrink rightward from here, so a
    /// change in its width moves every control from this x on.
    pub(crate) edge: f64,
    /// Where its controls sit once it has opened: the menu-aim target while
    /// it is open, and the spot that never opens it while it is not.
    pub(crate) controls: Rect,
}

impl Item {
    fn moves(&self, control: Rect) -> bool {
        control.x0 >= self.edge
    }
}

/// What the host knows about one pointer sample.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Sample<'a> {
    pub(crate) p: Point,
    /// The item under the pointer.
    pub(crate) under: Option<Item>,
    /// The item that is open now, if any ([`HoverIntent::open`]).
    pub(crate) open: Option<Item>,
    /// Every control clickable right now, as its pointer target.
    pub(crate) controls: &'a [Rect],
}

/// Why an open item stays open while the pointer is outside it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hold {
    /// Heading for its controls, or for a control its collapse would move;
    /// the instant of the last sample that confirmed it.
    Aim(Instant),
    /// On a control its collapse would move: no time limit.
    OnControl,
}

/// Menu-aim and dwell state for one row of hover-opening items.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HoverIntent {
    /// The item opened by the pointer.
    open: Option<usize>,
    /// The item that opens at the given instant unless the pointer says
    /// otherwise first.
    pending: Option<(usize, Instant)>,
    hold: Option<Hold>,
    /// The item the last sample rested on, if it may open there (on its
    /// body, not on or at a control).
    rest: Option<usize>,
    /// Newest last.
    samples: Vec<Point>,
}

impl HoverIntent {
    /// The item whose controls the pointer has opened.
    pub(crate) fn open(&self) -> Option<usize> {
        self.open
    }

    /// Test fixture: as if the pointer had dwelt on `idx` long enough.
    #[cfg(test)]
    pub(crate) fn force_open(&mut self, idx: Option<usize>) {
        *self = Self::default();
        self.open = idx;
        self.rest = idx;
    }

    /// Forget everything, e.g. when the pointer leaves the surface.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// A pointer sample.
    pub(crate) fn on_motion(&mut self, s: &Sample<'_>, now: Instant) {
        let p = s.p;
        let apex = self.samples.first().copied();
        if self.samples.len() >= APEX_SAMPLES {
            self.samples.remove(0);
        }
        self.samples.push(p);
        self.settle(now);

        // A repeated position says nothing about direction.
        let from = apex.filter(|a| *a != p);
        let heading = |r: Rect| from.is_some_and(|a| in_aim_hull(p, a, r));
        let on_control = s.controls.iter().any(|r| r.contains(p));
        let under = s.under.map(|u| u.idx);
        self.rest = s
            .under
            .filter(|u| !on_control && !u.controls.contains(p))
            .map(|u| u.idx);

        if under.is_some() && under == self.open {
            self.pending = None;
            self.hold = None;
            return;
        }
        if let Some(open) = s.open.filter(|o| Some(o.idx) == self.open) {
            if s.controls.iter().any(|r| r.contains(p) && open.moves(*r)) {
                self.hold = Some(Hold::OnControl);
                self.pending = None;
                return;
            }
            let aimed =
                heading(open.controls) || s.controls.iter().any(|r| open.moves(*r) && heading(*r));
            if aimed {
                self.hold = Some(Hold::Aim(now));
                self.pending = None;
                return;
            }
            if matches!(self.hold, Some(Hold::Aim(_))) && from.is_none() {
                return;
            }
        }
        self.hold = None;
        self.open = None;
        self.pending = match s.under {
            None => None,
            Some(u) if on_control || u.controls.contains(p) => None,
            Some(u) if s.controls.iter().any(|r| u.moves(*r) && heading(*r)) => {
                // On its way to a control this item's opening would move:
                // wait until the pointer has clearly stopped heading there.
                Some((u.idx, now + AIM_HOLD))
            }
            Some(u) => match self.pending {
                Some((idx, due)) if idx == u.idx => Some((idx, due)),
                _ => Some((u.idx, now + DWELL)),
            },
        };
        self.settle(now);
    }

    /// Advance timers to `now` without a pointer sample: a matured dwell
    /// opens its item, a lapsed aim hold hands over to the item the pointer
    /// rests on.
    pub(crate) fn settle(&mut self, now: Instant) {
        if let Some(Hold::Aim(confirmed)) = self.hold
            && now.saturating_duration_since(confirmed) >= AIM_HOLD
        {
            // The pointer has rested for the whole hold, longer than the
            // dwell, so the body it rests on (if any) opens straight away.
            self.hold = None;
            self.open = self.rest;
            self.pending = None;
        }
        if let Some((idx, due)) = self.pending
            && now >= due
        {
            self.open = Some(idx);
            self.pending = None;
        }
    }

    /// When [`settle`](Self::settle) next changes something with no further
    /// input, so the host can sleep until then instead of pumping frames.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        let dwell = self.pending.map(|(_, due)| due);
        let hold = match self.hold {
            Some(Hold::Aim(at)) => Some(at + AIM_HOLD),
            _ => None,
        };
        dwell.into_iter().chain(hold).min()
    }
}

/// Whether `p` lies in the convex hull of `apex` and `target`: inside the
/// target, or inside a triangle from the apex to one of its edges.
fn in_aim_hull(p: Point, apex: Point, target: Rect) -> bool {
    if target.contains(p) {
        return true;
    }
    let corners = [
        Point::new(target.x0, target.y0),
        Point::new(target.x1, target.y0),
        Point::new(target.x1, target.y1),
        Point::new(target.x0, target.y1),
    ];
    (0..corners.len()).any(|i| in_triangle(p, apex, corners[i], corners[(i + 1) % corners.len()]))
}

fn in_triangle(p: Point, a: Point, b: Point, c: Point) -> bool {
    let cross =
        |o: Point, u: Point, v: Point| (u.x - o.x) * (v.y - o.y) - (u.y - o.y) * (v.x - o.x);
    let d1 = cross(p, a, b);
    let d2 = cross(p, b, c);
    let d3 = cross(p, c, a);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    /// Two 100 px items side by side on a 20 px tall row; item 0 shows its
    /// × (x 70..100) at rest, and opening it would place its chips over
    /// x 50..100.
    fn item(idx: usize) -> Item {
        let edge = 110.0 * f64::from(u32::try_from(idx).unwrap());
        Item {
            idx,
            edge,
            controls: Rect::new(edge + 50.0, 0.0, edge + 100.0, 20.0),
        }
    }
    const CLOSE_0: Rect = Rect::new(70.0, 0.0, 100.0, 20.0);

    fn sample(
        p: Point,
        under: Option<usize>,
        open: Option<usize>,
        controls: &[Rect],
    ) -> Sample<'_> {
        Sample {
            p,
            under: under.map(item),
            open: open.map(item),
            controls,
        }
    }

    #[test]
    fn the_hull_covers_the_cone_toward_the_target_and_nothing_behind_it() {
        let target = Rect::new(100.0, 0.0, 120.0, 20.0);
        let apex = Point::new(0.0, 10.0);
        assert!(in_aim_hull(Point::new(50.0, 10.0), apex, target));
        assert!(in_aim_hull(Point::new(50.0, 6.0), apex, target));
        assert!(in_aim_hull(Point::new(110.0, 5.0), apex, target));
        assert!(
            !in_aim_hull(Point::new(50.0, 30.0), apex, target),
            "off the cone"
        );
        assert!(
            !in_aim_hull(Point::new(-5.0, 10.0), apex, target),
            "behind the apex"
        );
        assert!(
            !in_aim_hull(Point::new(130.0, 10.0), apex, target),
            "past the target"
        );
    }

    #[test]
    fn an_item_opens_after_the_dwell_from_entry_however_the_pointer_moves_in_it() {
        let t0 = crate::time::now();
        let mut h = HoverIntent::default();
        h.on_motion(&sample(Point::new(30.0, 10.0), Some(0), None, &[]), t0);
        assert_eq!(h.open(), None);
        assert_eq!(h.deadline(), Some(t0 + DWELL));
        // Steady motion inside the body, back and forth, away from its
        // controls, does not restart the dwell.
        for step in 1..10_u32 {
            let x = 10.0 + f64::from(step % 3) * 8.0;
            let at = ms(t0, u64::from(step) * 10);
            h.on_motion(&sample(Point::new(x, 10.0), Some(0), None, &[]), at);
        }
        h.settle(t0 + DWELL);
        assert_eq!(h.open(), Some(0));
        assert_eq!(h.deadline(), None, "nothing left to wake for");
    }

    #[test]
    fn a_sweep_across_items_opens_none_of_them() {
        let t0 = crate::time::now();
        let mut h = HoverIntent::default();
        for step in 0..8_u64 {
            let x = step as f64 * 30.0;
            let under = [item(0), item(1)]
                .into_iter()
                .find(|i| x >= i.edge && x < i.edge + 100.0)
                .map(|i| i.idx);
            h.on_motion(
                &sample(Point::new(x, 10.0), under, None, &[]),
                ms(t0, step * 25),
            );
            assert_eq!(h.open(), None, "opened mid-sweep at x {x}");
        }
        h.on_motion(
            &sample(Point::new(400.0, 200.0), None, None, &[]),
            ms(t0, 300),
        );
        h.settle(ms(t0, 2000));
        assert_eq!(h.open(), None);
    }

    #[test]
    fn resting_on_a_control_or_where_the_chips_will_be_never_opens() {
        let t0 = crate::time::now();
        for p in [Point::new(85.0, 10.0), Point::new(60.0, 10.0)] {
            let mut h = HoverIntent::default();
            h.on_motion(&sample(p, Some(0), None, &[CLOSE_0]), t0);
            h.settle(ms(t0, 10_000));
            assert_eq!(h.open(), None, "opened while resting at {p:?}");
            assert_eq!(h.deadline(), None, "nothing to wake for at {p:?}");
        }
    }

    #[test]
    fn heading_for_the_close_control_across_the_body_does_not_open() {
        let t0 = crate::time::now();
        let mut h = HoverIntent::default();
        // A slow approach: 1 px every 8 ms, from the leading edge to the ×.
        let mut t = 0;
        for x in 5..=85_u32 {
            let p = Point::new(f64::from(x), 10.0);
            h.on_motion(&sample(p, Some(0), None, &[CLOSE_0]), ms(t0, t));
            assert_eq!(h.open(), None, "opened on the way to the × at x {x}");
            t += 8;
        }
        h.settle(ms(t0, t + 10_000));
        assert_eq!(h.open(), None);
    }

    #[test]
    fn stopping_on_the_body_after_heading_for_a_control_opens_once_the_hold_lapses() {
        let t0 = crate::time::now();
        let mut h = HoverIntent::default();
        for (k, x) in [5.0, 15.0, 25.0, 35.0].into_iter().enumerate() {
            let at = ms(t0, u64::try_from(k).unwrap() * 8);
            h.on_motion(&sample(Point::new(x, 10.0), Some(0), None, &[CLOSE_0]), at);
        }
        let last = ms(t0, 24);
        h.settle(last + DWELL);
        assert_eq!(
            h.open(),
            None,
            "a dwell is not enough after heading for the ×"
        );
        h.settle(last + AIM_HOLD);
        assert_eq!(h.open(), Some(0));
    }

    #[test]
    fn an_open_item_holds_while_the_pointer_rests_on_a_control_its_collapse_would_move() {
        let t0 = crate::time::now();
        let mut h = HoverIntent::default();
        h.force_open(Some(0));
        let close_1 = Rect::new(180.0, 0.0, 210.0, 20.0);
        let controls = [close_1];
        // Out of item 0, across the gap, onto item 1's ×.
        for (k, x) in [95.0, 105.0, 150.0, 195.0].into_iter().enumerate() {
            let p = Point::new(x, 10.0);
            let under = if x < 100.0 {
                Some(0)
            } else if x >= 110.0 {
                Some(1)
            } else {
                None
            };
            let at = ms(t0, u64::try_from(k).unwrap() * 8);
            h.on_motion(&sample(p, under, Some(0), &controls), at);
            assert_eq!(h.open(), Some(0), "collapsed under the pointer at x {x}");
        }
        h.settle(ms(t0, 60_000));
        assert_eq!(h.open(), Some(0), "the × under the pointer must not move");
    }
}
