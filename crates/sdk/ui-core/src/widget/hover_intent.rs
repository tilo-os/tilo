//! Pointer intent for controls that open on hover: a short dwell before
//! anything opens, and menu-aim prediction while the pointer travels to
//! what has opened.
//!
//! Two failure modes pull in opposite directions:
//!
//! - A pointer merely crossing a row of items must not open each one it
//!   passes over. A short dwell filters that out: an item opens once the
//!   pointer has been over it for [`DWELL`], measured from when it entered,
//!   whatever the pointer does inside it meanwhile.
//! - A pointer heading for controls that have already opened must not close
//!   them by grazing a neighbour or the gap on the way. As in a mega menu's
//!   submenu prediction, each motion sample is checked against the triangle
//!   (the convex hull) spanned by a recent pointer position and the target:
//!   while the pointer keeps moving inside it, the opened item stays open
//!   and nothing else starts to open. Once it stops for [`AIM_HOLD`], or
//!   moves anywhere outside it, the hold ends.
//!
//! The state is pure: every method takes the instant it reasons about, so
//! slow and fast pointer paths are tested against a synthetic timeline.

use crate::time::{Duration, Instant};
use vello::kurbo::{Point, Rect};

/// How long the pointer rests over an item before it opens. Shorter than a
/// deliberate pause, longer than the time a pointer sweeping across a row
/// spends over any one item.
pub(crate) const DWELL: Duration = Duration::from_millis(100);

/// How long an aimed-at item stays open once the pointer stops moving
/// toward it without arriving.
pub(crate) const AIM_HOLD: Duration = Duration::from_millis(300);

/// Recent pointer positions kept; the oldest is the triangle's apex, so a
/// single jittery sample cannot point the cone the wrong way.
const APEX_SAMPLES: usize = 3;

/// Dwell plus menu-aim state for one row of hover-opening items, keyed by
/// index.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HoverIntent {
    /// The item opened by the pointer.
    open: Option<usize>,
    /// The item under the pointer and when the pointer entered it.
    pending: Option<(usize, Instant)>,
    /// While the pointer travels toward `open`'s target outside `open`: the
    /// last sample that confirmed the aim.
    aim_confirmed_at: Option<Instant>,
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
    }

    /// Forget everything, e.g. when the pointer leaves the surface.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// A pointer sample at `p`, over item `under` (if any). `target` is the
    /// open item's controls, the region the pointer may be heading for.
    pub(crate) fn on_motion(
        &mut self,
        p: Point,
        under: Option<usize>,
        target: Option<Rect>,
        now: Instant,
    ) {
        let apex = self.samples.first().copied();
        if self.samples.len() >= APEX_SAMPLES {
            self.samples.remove(0);
        }
        self.samples.push(p);
        self.settle(under, now);

        if under.is_some() && under == self.open {
            self.pending = None;
            self.aim_confirmed_at = None;
            return;
        }
        let aimed = self.open.is_some()
            && match (apex, target) {
                (Some(apex), Some(target)) if apex != p => in_aim_hull(p, apex, target),
                _ => false,
            };
        if aimed {
            // Heading for the open controls: hold everything as it is.
            self.aim_confirmed_at = Some(now);
            self.pending = None;
            return;
        }
        if self.aim_confirmed_at.is_some() && apex == Some(p) {
            // A repeated position says nothing about direction.
            return;
        }
        self.aim_confirmed_at = None;
        self.open = None;
        self.pending = match (self.pending, under) {
            (Some((idx, since)), Some(u)) if idx == u => Some((idx, since)),
            (_, Some(u)) => Some((u, now)),
            (_, None) => None,
        };
        self.settle(under, now);
    }

    /// Advance timers to `now` without a pointer sample: a matured dwell
    /// opens its item, a lapsed aim hold hands over to whatever the pointer
    /// rests on.
    pub(crate) fn settle(&mut self, under: Option<usize>, now: Instant) {
        if let Some(confirmed) = self.aim_confirmed_at
            && now.saturating_duration_since(confirmed) >= AIM_HOLD
        {
            // The pointer has rested over `under` for the whole hold, which
            // is longer than the dwell, so it opens straight away.
            self.aim_confirmed_at = None;
            self.open = under;
            self.pending = None;
        }
        if let Some((idx, since)) = self.pending
            && now.saturating_duration_since(since) >= DWELL
        {
            self.open = Some(idx);
            self.pending = None;
        }
    }

    /// When [`settle`](Self::settle) next changes something with no further
    /// input, so the host can sleep until then instead of pumping frames.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        let dwell = self.pending.map(|(_, since)| since + DWELL);
        let hold = self.aim_confirmed_at.map(|at| at + AIM_HOLD);
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
        h.on_motion(Point::new(10.0, 10.0), Some(0), None, t0);
        assert_eq!(h.open(), None);
        assert_eq!(h.deadline(), Some(t0 + DWELL));
        // Steady motion inside the item does not restart the dwell.
        for step in 1..10 {
            let at = ms(t0, step * 10);
            h.on_motion(
                Point::new(10.0 + step as f64 * 8.0, 10.0),
                Some(0),
                None,
                at,
            );
        }
        h.settle(Some(0), t0 + DWELL);
        assert_eq!(h.open(), Some(0));
        assert_eq!(h.deadline(), None, "nothing left to wake for");
    }

    #[test]
    fn a_sweep_across_items_opens_none_of_them() {
        let t0 = crate::time::now();
        let mut h = HoverIntent::default();
        for step in 0..12_u64 {
            let item = usize::try_from(step / 3).unwrap();
            h.on_motion(
                Point::new(step as f64 * 30.0, 10.0),
                Some(item),
                None,
                ms(t0, step * 25),
            );
            assert_eq!(h.open(), None, "item {item} opened mid-sweep");
        }
        h.on_motion(Point::new(400.0, 200.0), None, None, ms(t0, 300));
        h.settle(None, ms(t0, 2000));
        assert_eq!(h.open(), None);
    }
}
