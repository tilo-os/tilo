//! Pointer intent for a row of items that open on hover to show more
//! controls (a tab pill's search/maximize chips beside its ×).
//!
//! Opening an item changes the row's geometry: the item grows rightward
//! from its leading edge, so its own controls and everything after it move.
//! The rules, in priority order:
//!
//! 1. **An aimed-at control never moves.** A pointer that settles on a
//!    control (a ×, a revealed chip, "new tab") is aiming at it: its speed
//!    drops toward zero with the trajectory ending inside the control. Until
//!    it leaves, nothing opens or collapses that would move that control, so
//!    the × stays under a pointer about to press it.
//! 2. **Passing over a control is not hovering it.** A pointer still moving
//!    with its trajectory continuing past the control is in transit: the
//!    control lights no hover wash and holds nothing back, so a pointer that
//!    crosses the × on its way to the chips still opens the pill. Whether a
//!    pointer inside a control is passing or settling is only known once it
//!    leaves or stops, so a change that would move that control waits at
//!    most [`SETTLE`] for the answer.
//! 3. **Heading for a control the change would move holds the change.** Each
//!    motion sample from outside a control is checked against the triangle
//!    (the convex hull) spanned by a recent pointer position and the control.
//!    An item crossed on the way to its own × does not open under the
//!    approaching pointer, and an open item does not collapse (sliding its
//!    neighbours) while the pointer heads for a neighbour's ×.
//! 4. **Seeing more takes a short rest on the item**: [`DWELL`], counted from
//!    when the pointer entered it (a sweep across the row spends less than
//!    that on each item, so none opens). Once open, the item stays open while
//!    the pointer is in it or heading for its revealed controls (mega-menu
//!    "menu aim", rather than a timer long enough for the slowest hand), and
//!    closes the moment the pointer moves anywhere else.
//!
//! A change held by heading resumes [`AIM_HOLD`] after the last sample that
//! confirmed the heading, so a pointer that stops short lets go instead of
//! pinning the row.
//!
//! Transit and aim are read from the system's one pointer predictor,
//! [`super::aim::Trajectory`].
//!
//! The state is pure: every method takes the instant it reasons about, so
//! slow and fast pointer paths are tested against a synthetic timeline.

use super::aim::Trajectory;
pub(crate) use super::aim::{SETTLE, in_aim_hull};
use crate::time::{Duration, Instant};
use vello::kurbo::{Point, Rect};

/// How long the pointer rests on an item before it opens. Shorter than a
/// deliberate pause, longer than the time a pointer sweeping across a row
/// spends over any one item.
pub(crate) const DWELL: Duration = Duration::from_millis(100);

/// How long a change held by heading waits once the pointer stops moving
/// toward what it was heading for without arriving.
pub(crate) const AIM_HOLD: Duration = Duration::from_millis(300);

/// One item of the row, as laid out now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Item {
    /// The host's key for the item.
    pub(crate) idx: usize,
    /// Where it starts. Items grow and shrink rightward from here, so a
    /// change in its width moves every control from this x on.
    pub(crate) edge: f64,
    /// Where its controls sit once it has opened: the menu-aim target.
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
    /// In a control its collapse would move: held until the pointer leaves.
    InControl,
}

/// Where the last sample was, as far as the timers care.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Last {
    at: Instant,
    settling: bool,
    /// The sample was inside a control, whose hover wash waits on whether
    /// the pointer settles there.
    in_control: bool,
    /// The sample was inside a control that opening the pending item would
    /// move.
    in_moved_control: bool,
}

/// Menu-aim and dwell state for one row of hover-opening items.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HoverIntent {
    open: Option<usize>,
    /// The item that opens at the given instant, unless the pointer is
    /// aiming at a control the opening would move by then.
    pending: Option<(usize, Instant)>,
    hold: Option<Hold>,
    /// The item the last sample was over.
    rest: Option<usize>,
    last: Option<Last>,
    path: Trajectory,
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

    /// Whether the pointer is settled (aiming at whatever it is on) rather
    /// than passing through. A control's hover wash follows this, so a
    /// control the pointer only crosses never lights up. Without motion seen
    /// yet, the pointer is where it is.
    pub(crate) fn settled(&self, now: Instant) -> bool {
        self.last
            .is_none_or(|l| l.settling || now.saturating_duration_since(l.at) >= SETTLE)
    }

    /// A pointer sample.
    pub(crate) fn on_motion(&mut self, s: &Sample<'_>, now: Instant) {
        let p = s.p;
        self.path.push(p, now);
        self.settle(now);

        // A repeated position says nothing about direction.
        let from = self.path.apex().filter(|a| *a != p);
        let toward = |r: Rect| from.is_some_and(|a| in_aim_hull(p, a, r));
        // From inside a clickable control only settling counts, never
        // heading: a pointer already on it is passing or aiming.
        let heading = |r: Rect| !r.contains(p) && toward(r);
        let under = s.under.map(|u| u.idx);
        let control = s.controls.iter().copied().find(|r| r.contains(p));
        let settling = self.path.settling(control);
        self.rest = under;
        self.last = Some(Last {
            at: now,
            settling,
            in_control: control.is_some(),
            in_moved_control: s
                .under
                .zip(control)
                .is_some_and(|(u, r)| Some(u.idx) != self.open && u.moves(r)),
        });

        if under.is_some() && under == self.open {
            self.pending = None;
            self.hold = None;
            return;
        }
        if let Some(open) = s.open.filter(|o| Some(o.idx) == self.open) {
            if control.is_some_and(|r| open.moves(r)) {
                self.hold = Some(Hold::InControl);
                self.pending = None;
                return;
            }
            let aimed =
                toward(open.controls) || s.controls.iter().any(|r| open.moves(*r) && heading(*r));
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
    /// opens its item unless the pointer is aiming at (or may be settling
    /// on) a control that would move; a lapsed aim hold hands over to the
    /// item the pointer rests on.
    pub(crate) fn settle(&mut self, now: Instant) {
        if let Some(Hold::Aim(confirmed)) = self.hold
            && now.saturating_duration_since(confirmed) >= AIM_HOLD
        {
            // The pointer has rested for the whole hold, longer than the
            // dwell, so the item it rests on opens straight away, unless it
            // rests on a control that would move.
            self.hold = None;
            self.open = None;
            self.pending = self.rest.map(|idx| (idx, now));
        }
        if let Some((idx, due)) = self.pending
            && now >= due
            && !self.last.is_some_and(|l| l.in_moved_control)
        {
            self.open = Some(idx);
            self.pending = None;
        }
    }

    /// When [`settle`](Self::settle) or [`settled`](Self::settled) next
    /// changes with no further input, so the host can sleep until then
    /// instead of pumping frames. A pending item blocked by a control the
    /// pointer has settled on has nothing to wait for.
    pub(crate) fn deadline(&self, now: Instant) -> Option<Instant> {
        let unsettled = self
            .last
            .filter(|l| l.in_control && !l.settling)
            .map(|l| l.at + SETTLE)
            .filter(|at| *at > now);
        let dwell = self
            .pending
            .filter(|_| !self.last.is_some_and(|l| l.in_moved_control))
            .map(|(_, due)| due);
        let hold = match self.hold {
            Some(Hold::Aim(at)) => Some(at + AIM_HOLD),
            _ => None,
        };
        unsettled.into_iter().chain(dwell).chain(hold).min()
    }
}

/// Reaching for something that waits at a display's edge (a desk widget's
/// quiet form): the same transit-versus-aim reading as `HoverIntent`,
/// for targets that are not under the pointer yet.
///
/// A sample aims at a target when both hold:
///
/// - **it heads there**: it lies in the convex hull of a recent position and
///   the target, so a pointer moving away or sideways past it does not;
/// - **it is coming to rest there**: it is already slow inside the target,
///   or it is decelerating so that, at constant deceleration, it stops inside
///   the target.
///
/// A pointer thrown at the edge stops there without slowing first, and the
/// display then sends no more motion: [`EdgeAim::rested_in`] reads that
/// rest once the pointer has been still for [`SETTLE`] inside a target it
/// arrived at from outside.
///
/// A pointer sweeping across or along a target at speed is in transit and
/// aims at nothing, however close it passes; one predicted to run on past
/// the display's edge is not coming to rest anywhere yet; and one that has
/// not moved aims at nothing either, so a pointer resting near an edge never
/// counts.
///
/// The state is pure: every method takes the instant it reasons about.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EdgeAim {
    path: Trajectory,
}

impl EdgeAim {
    /// Forget the trajectory, e.g. when the pointer leaves the display or
    /// jumps to another one.
    pub fn clear(&mut self) {
        self.path.clear();
    }

    /// A pointer sample at `p`. Returns the index of the target in `targets`
    /// the pointer is aiming at, the first one when several qualify.
    pub fn on_motion(&mut self, p: Point, now: Instant, targets: &[Rect]) -> Option<usize> {
        self.path.push(p, now);
        let from = self.path.apex().filter(|a| *a != p)?;
        targets
            .iter()
            .position(|target| in_aim_hull(p, from, *target) && self.path.comes_to_rest_in(*target))
    }

    /// The target the pointer has come to rest in at `now`: still for
    /// [`SETTLE`] inside it, having arrived from outside it. A throw into a
    /// corner ends this way, with no slow sample to read.
    #[must_use]
    pub fn rested_in(&self, now: Instant, targets: &[Rect]) -> Option<usize> {
        self.path.rested_in(now, targets)
    }

    /// Whether the pointer is resting where it is: slow at its last sample,
    /// or silent for [`SETTLE`] since. Without motion seen, it is.
    #[must_use]
    pub fn settled(&self, now: Instant) -> bool {
        self.path.settled(now)
    }

    /// When [`Self::settled`] next changes with no further input.
    #[must_use]
    pub fn deadline(&self, now: Instant) -> Option<Instant> {
        self.path.settle_deadline(now)
    }
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
        assert_eq!(h.deadline(t0), Some(t0 + DWELL));
        // Steady motion inside the body, back and forth, away from its
        // controls, does not restart the dwell.
        for step in 1..10_u32 {
            let x = 10.0 + f64::from(step % 3) * 8.0;
            let at = ms(t0, u64::from(step) * 10);
            h.on_motion(&sample(Point::new(x, 10.0), Some(0), None, &[]), at);
        }
        h.settle(t0 + DWELL);
        assert_eq!(h.open(), Some(0));
        assert_eq!(
            h.deadline(t0 + DWELL + SETTLE),
            None,
            "nothing left to wake for"
        );
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
    fn settling_on_a_control_never_opens_and_its_body_does() {
        let t0 = crate::time::now();
        let mut h = HoverIntent::default();
        h.on_motion(
            &sample(Point::new(85.0, 10.0), Some(0), None, &[CLOSE_0]),
            t0,
        );
        h.settle(ms(t0, 10_000));
        assert_eq!(h.open(), None, "opened under a pointer resting on the ×");
        assert_eq!(h.deadline(ms(t0, 10_000)), None, "nothing to wake for");

        // Where the chips will sit is just the body until they are there.
        let mut h = HoverIntent::default();
        h.on_motion(
            &sample(Point::new(60.0, 10.0), Some(0), None, &[CLOSE_0]),
            t0,
        );
        h.settle(t0 + DWELL);
        assert_eq!(h.open(), Some(0));
    }

    /// Entering through the × at speed, on the way to the body: transit, not
    /// hovering. The pill opens on the dwell from entry as if the × were
    /// not there.
    #[test]
    fn crossing_the_close_control_in_transit_still_opens() {
        let t0 = crate::time::now();
        let mut h = HoverIntent::default();
        // Leftward at 2 px/ms from item 1, through item 0's ×, onto its body.
        let mut entered = None;
        for (k, x) in [124.0, 108.0, 92.0, 76.0, 60.0, 44.0, 28.0]
            .into_iter()
            .enumerate()
        {
            let at = ms(t0, u64::try_from(k).unwrap() * 8);
            let under = if x < 100.0 {
                Some(0)
            } else if x >= 110.0 {
                Some(1)
            } else {
                None
            };
            if under == Some(0) && entered.is_none() {
                entered = Some(at);
            }
            let p = Point::new(x, 10.0);
            h.on_motion(&sample(p, under, None, &[CLOSE_0]), at);
            if CLOSE_0.contains(p) {
                assert!(!h.settled(at), "crossing the × at speed is not hovering it");
            }
        }
        let entered = entered.unwrap();
        h.settle(entered + DWELL);
        assert_eq!(h.open(), Some(0), "the dwell counts from entry, × included");
    }

    /// A pointer drifting through the × whose dwell runs out while it is
    /// still inside: the pill waits for it to leave (it was passing) rather
    /// than move the × under a pointer that may be about to stop there.
    #[test]
    fn a_dwell_that_ends_inside_a_control_waits_for_the_pointer_to_leave() {
        let t0 = crate::time::now();
        let mut h = HoverIntent::default();
        // Downward through the × at 0.1 px/ms, a sample every 10 ms.
        let mut t = 0;
        for y in 0..=19_u32 {
            let p = Point::new(85.0, f64::from(y) * 1.0 + 0.5);
            h.on_motion(&sample(p, Some(0), None, &[CLOSE_0]), ms(t0, t));
            assert_eq!(h.open(), None, "opened with the pointer on the × at {t} ms");
            t += 10;
        }
        h.on_motion(
            &sample(Point::new(85.0, 21.0), Some(0), None, &[]),
            ms(t0, t),
        );
        assert_eq!(h.open(), Some(0), "opens the moment the pointer is off it");
    }

    /// Speed falling off so fast that the pointer will stop inside the × is
    /// aim already, before it has come to rest.
    #[test]
    fn decelerating_into_a_control_is_aim() {
        let t0 = crate::time::now();
        let mut h = HoverIntent::default();
        let path = [(40.0, 0), (64.0, 8), (78.0, 16), (84.0, 24)];
        for (x, t) in path {
            h.on_motion(
                &sample(Point::new(x, 10.0), Some(0), None, &[CLOSE_0]),
                ms(t0, t),
            );
        }
        assert!(
            h.settled(ms(t0, 24)),
            "3 px/ms, then 1.75, then 0.75: stopping"
        );
        // A pointer at a steady 3 px/ms through the same × is passing.
        let mut h = HoverIntent::default();
        for (k, x) in [40.0, 64.0, 88.0].into_iter().enumerate() {
            let at = ms(t0, u64::try_from(k).unwrap() * 8);
            h.on_motion(&sample(Point::new(x, 10.0), Some(0), None, &[CLOSE_0]), at);
        }
        assert!(!h.settled(ms(t0, 16)));
        assert!(h.settled(ms(t0, 16) + SETTLE), "until it goes quiet");
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

    /// A widget's reach zone in the bottom-right corner of a 1600 × 900
    /// display and another along the middle of its right edge.
    const CORNER: Rect = Rect::new(1340.0, 820.0, 1600.0, 900.0);
    const RIGHT: Rect = Rect::new(1540.0, 350.0, 1600.0, 550.0);

    /// Feed `path` 8 ms apart; the aimed target after each sample.
    fn feed(aim: &mut EdgeAim, t0: Instant, path: &[(f64, f64)]) -> Vec<Option<usize>> {
        path.iter()
            .enumerate()
            .map(|(k, (x, y))| {
                let at = ms(t0, u64::try_from(k).unwrap() * 8);
                aim.on_motion(Point::new(*x, *y), at, &[CORNER, RIGHT])
            })
            .collect()
    }

    #[test]
    fn sweeping_along_an_edge_past_a_widget_is_transit_not_aim() {
        // Down the right edge at a steady 2 px/ms, straight through the
        // right widget's zone and on toward the bottom.
        let path: Vec<(f64, f64)> = (0..40)
            .map(|k| (1590.0, 100.0 + 16.0 * f64::from(k)))
            .collect();
        let mut aim = EdgeAim::default();
        let aimed = feed(&mut aim, crate::time::now(), &path[..30]);
        assert!(
            aimed.iter().all(Option::is_none),
            "passing over the right widget must not count as aim: {aimed:?}"
        );
    }

    #[test]
    fn crossing_the_screen_past_a_corner_zone_is_transit() {
        let path: Vec<(f64, f64)> = (0..12)
            .map(|k| (1500.0 - 30.0 * f64::from(k), 860.0 - 10.0 * f64::from(k)))
            .collect();
        let mut aim = EdgeAim::default();
        assert!(
            feed(&mut aim, crate::time::now(), &path)
                .iter()
                .all(Option::is_none)
        );
    }

    #[test]
    fn slowing_toward_a_corner_widget_is_aim_before_the_pointer_arrives() {
        // From the middle toward the bottom-right corner, decelerating so the
        // stop lands in the corner zone while the pointer is still outside.
        let mut aim = EdgeAim::default();
        let path = [(1000.0, 560.0), (1160.0, 656.0), (1280.0, 728.0)];
        let aimed = feed(&mut aim, crate::time::now(), &path);
        assert_eq!(aimed.last(), Some(&Some(0)), "{aimed:?}");
        assert!(!CORNER.contains(Point::new(1280.0, 728.0)));
    }

    #[test]
    fn a_throw_into_the_corner_aims_once_it_stops_at_the_edge() {
        let mut aim = EdgeAim::default();
        // Steady and fast (no deceleration to read), then pinned at the
        // display's corner where the pointer cannot go further.
        let path = [
            (1300.0, 700.0),
            (1400.0, 760.0),
            (1500.0, 820.0),
            (1599.0, 899.0),
            (1599.0, 899.0),
        ];
        let aimed = feed(&mut aim, crate::time::now(), &path);
        assert_eq!(aimed[..3], [None, None, None], "{aimed:?}");
        assert!(aimed[3..].contains(&Some(0)), "{aimed:?}");
    }

    #[test]
    fn a_pointer_resting_near_an_edge_aims_at_nothing() {
        let mut aim = EdgeAim::default();
        let t0 = crate::time::now();
        for k in 0..6 {
            assert_eq!(
                aim.on_motion(Point::new(1560.0, 450.0), ms(t0, k * 8), &[RIGHT]),
                None
            );
        }
        assert!(aim.settled(ms(t0, 100)));
        assert_eq!(
            aim.rested_in(ms(t0, 100), &[RIGHT]),
            None,
            "it never arrived"
        );
    }

    #[test]
    fn slowing_down_an_edge_toward_its_corner_is_not_yet_aim() {
        // Down the right edge, easing off but still fast enough to run past
        // the display's bottom: no stop is predicted inside the corner.
        let mut aim = EdgeAim::default();
        let path = [(1590.0, 400.0), (1590.0, 560.0), (1590.0, 700.0)];
        assert!(
            feed(&mut aim, crate::time::now(), &path)
                .iter()
                .all(Option::is_none)
        );
    }

    #[test]
    fn a_pointer_stopped_by_the_corner_has_rested_there() {
        let t0 = crate::time::now();
        let mut aim = EdgeAim::default();
        let path = [(1300.0, 700.0), (1450.0, 790.0), (1599.0, 899.0)];
        assert!(
            feed(&mut aim, t0, &path).iter().all(Option::is_none),
            "still fast"
        );
        assert_eq!(
            aim.rested_in(ms(t0, 20), &[CORNER, RIGHT]),
            None,
            "not still yet"
        );
        assert_eq!(
            aim.rested_in(ms(t0, 16) + SETTLE, &[CORNER, RIGHT]),
            Some(0)
        );
    }

    #[test]
    fn heading_away_from_a_widget_while_slowing_is_not_aim() {
        let mut aim = EdgeAim::default();
        let path = [
            (1500.0, 860.0),
            (1300.0, 740.0),
            (1160.0, 656.0),
            (1070.0, 602.0),
        ];
        assert!(
            feed(&mut aim, crate::time::now(), &path)
                .iter()
                .all(Option::is_none)
        );
    }
}
