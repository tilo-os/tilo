//! Pointer intent for chrome that hides along a window's top edge until the person reaches for
//! it: the decorations of a maximized window (its title, Full screen, Restore and close) and,
//! in a Tilo app that puts its header there, the app's header row with them as one bar.
//!
//! A maximized window gives its whole height to the app, so the bar hides at rest and appears
//! *over* the window's top edge; the hard part is telling "click something at the top of the
//! app" from "reach the window's bar". The rules, in priority order:
//!
//! 1. **Only the very top asks for the bar**: the reveal band, the top [`REVEAL_BAND_PX`] of
//!    the window, above every control an app puts in its top row.
//! 2. **Transit is not aim**: a sample is *transit* while the pointer moves faster than
//!    [`TRANSIT_SPEED`] without braking (still above [`BRAKE_RATIO`] of its recent peak).
//! 3. **Pushing to the edge is intent**: braking in the band after a fast climb (from
//!    [`PUSH_TRAVEL_PX`] below within [`PUSH_WINDOW`], steeper than 45°) shows the bar at once.
//! 4. **Otherwise it takes a short rest** in the band, [`DWELL`]; a pointer coming down from
//!    the desktop's top bar gets no push credit.
//! 5. **A click in the band while the bar is hidden belongs to the app**, cancelling a
//!    pending reveal until the pointer leaves the band.
//! 6. **While shown, the bar stays** while the pointer settles on it or in the band, or heads
//!    for one of its controls (menu aim, lapsing [`AIM_HOLD`] after the last confirming
//!    sample); an overshoot into the desktop's top bar gets [`OVERSHOOT_GRACE`] to come back.
//! 7. **Aimed-at controls don't move**: showing and hiding only fade the bar in place.
//! 8. **Keyboard and touch reach it too**: [`TopEdgeReveal::pin`] until
//!    [`TopEdgeReveal::unpin`], or a touch that drags down [`TOUCH_SWIPE_PX`].
//!
//! The desktop's own top bar shares this model so the two never fight over the edge: both
//! hidden, approaching the top reveals the top stack together, and while the desktop's bar
//! shows over the window's top edge the window's bar stands below it. Transit, braking and
//! heading come from [`super::aim::Trajectory`]. The state is pure: every method takes the
//! instant it reasons about, so pointer paths are tested against a synthetic timeline.

use super::aim::{Trajectory, in_aim_hull};
use crate::time::{Duration, Instant};
use vello::kurbo::{Point, Rect};

/// Height of the band at the window's top that summons the bar, in logical
/// px. Thinner than the inset any header row keeps above its controls.
pub const REVEAL_BAND_PX: f64 = crate::tokens::Metrics::BASE.icon_label_gap as f64;

pub const DWELL: Duration = Duration::from_millis(150);

/// How far below the band a pushing pointer started, at most
/// [`PUSH_WINDOW`] ago.
pub const PUSH_TRAVEL_PX: f64 = crate::tokens::Spacing::BASE.s12 as f64;

/// How recent the start of a push must be.
pub const PUSH_WINDOW: Duration = Duration::from_millis(160);

pub const AIM_HOLD: Duration = super::hover_intent::AIM_HOLD;

pub const OVERSHOOT_GRACE: Duration = Duration::from_millis(300);

/// Pointer speed (logical px per ms) above which a sample that is not
/// braking is transit: on its way somewhere else.
pub const TRANSIT_SPEED: f64 = 0.6;

pub const BRAKE_RATIO: f64 = 0.6;

/// Peak speed a climb must reach to count as a push for the edge.
pub const PUSH_SPEED: f64 = crate::tokens::motion::PUSH_SPEED;

pub const TOUCH_SWIPE_PX: f64 = crate::tokens::Spacing::BASE.s6 as f64;

/// The band at a display's top edge a finger swipes down from: wider than
/// the pointer's [`REVEAL_BAND_PX`], since a finger coming off the bezel
/// lands a few pixels in.
pub const TOUCH_EDGE_PX: f64 = crate::tokens::Spacing::BASE.s3 as f64;

#[derive(Clone, Copy, Debug)]
pub struct EdgeGeometry<'a> {
    pub window: Rect,
    /// What the shown bar paints over the window (its chips, or a whole
    /// header row): a pointer on any of them keeps the bar.
    pub bar: &'a [Rect],
    pub controls: &'a [Rect],
}

impl EdgeGeometry<'_> {
    #[must_use]
    pub fn band(&self) -> Rect {
        Rect::new(
            self.window.x0,
            self.window.y0,
            self.window.x1,
            (self.window.y0 + REVEAL_BAND_PX).min(self.window.y1),
        )
    }

    fn in_band(&self, p: Point) -> bool {
        let band = self.band();
        p.x >= band.x0 && p.x < band.x1 && p.y >= band.y0 && p.y < band.y1
    }

    fn above(&self, p: Point) -> bool {
        p.y < self.window.y0 && p.x >= self.window.x0 && p.x < self.window.x1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hold {
    /// Heading for a control; hides at the instant unless confirmed again.
    Aim(Instant),
    /// Overshot into the top bar; hides at the instant unless it returns.
    Overshoot(Instant),
}

impl Hold {
    fn until(self) -> Instant {
        match self {
            Self::Aim(at) | Self::Overshoot(at) => at,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TopEdgeReveal {
    shown: bool,
    /// The bar shows at this instant unless the pointer says otherwise.
    due: Option<Instant>,
    hold: Option<Hold>,
    pinned: bool,
    suppressed: bool,
    path: Trajectory,
    touch_from: Option<Point>,
}

impl TopEdgeReveal {
    #[must_use]
    pub fn shown(&self) -> bool {
        self.shown
    }

    fn set_shown(&mut self, shown: bool) -> bool {
        self.due = None;
        self.hold = None;
        if self.shown == shown {
            return false;
        }
        self.shown = shown;
        true
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn hide(&mut self) -> bool {
        self.pinned = false;
        self.set_shown(false)
    }

    pub fn pin(&mut self) -> bool {
        self.pinned = true;
        self.set_shown(true)
    }

    pub fn unpin(&mut self) {
        self.pinned = false;
    }

    pub fn reveal_by_touch(&mut self) -> bool {
        self.pinned = false;
        self.touch_from = None;
        self.set_shown(true)
    }

    pub fn on_motion(&mut self, p: Point, geo: &EdgeGeometry<'_>, now: Instant) -> bool {
        let mut changed = self.settle(now);
        if self.path.last().is_some_and(|(last, _)| last == p) {
            return changed;
        }
        let apexes: Vec<Point> = self
            .path
            .samples()
            .iter()
            .rev()
            .take(3)
            .map(|(point, _)| *point)
            .collect();
        self.path.push(p, now);
        let in_band = geo.in_band(p);
        if !in_band {
            self.suppressed = false;
        }
        let transit = self.path.in_transit(TRANSIT_SPEED, BRAKE_RATIO);
        if self.shown {
            let on_control = geo.controls.iter().any(|r| r.contains(p));
            let on_bar = on_control || in_band || geo.bar.iter().any(|r| r.contains(p));
            if on_bar {
                self.hold = None;
                return changed;
            }
            if geo.above(p) {
                if !matches!(self.hold, Some(Hold::Overshoot(_))) {
                    self.hold = Some(Hold::Overshoot(now + OVERSHOOT_GRACE));
                }
                return changed;
            }
            // Heading for a control it is not on yet; crossing one is transit.
            let heading = apexes.iter().filter(|a| **a != p).any(|a| {
                geo.controls
                    .iter()
                    .any(|r| !r.contains(p) && in_aim_hull(p, *a, *r))
            });
            if heading {
                self.hold = Some(Hold::Aim(now + AIM_HOLD));
                return changed;
            }
            if self.pinned {
                return changed;
            }
            changed |= self.set_shown(false);
            return changed;
        }
        if !in_band || self.suppressed {
            self.due = None;
            return changed;
        }
        if self.pushed_up(p, geo, now) {
            changed |= self.set_shown(true);
        } else if transit || self.due.is_none() {
            self.due = Some(now + DWELL);
        }
        changed
    }

    fn pushed_up(&self, p: Point, geo: &EdgeGeometry<'_>, now: Instant) -> bool {
        if !self.path.braking(PUSH_SPEED, BRAKE_RATIO) {
            return false;
        }
        let floor = geo.band().y1;
        // The path since the push started must stay inside the window: one
        // that came down from the desktop's bar is entering the app.
        for (q, at) in self.path.samples().iter().rev().skip(1) {
            if now.saturating_duration_since(*at) > PUSH_WINDOW {
                break;
            }
            if geo.above(*q) || !geo.window.contains(*q) {
                return false;
            }
            let rise = q.y - p.y;
            if q.y >= floor && rise >= PUSH_TRAVEL_PX && (q.x - p.x).abs() <= rise {
                return true;
            }
        }
        false
    }

    pub fn on_press(&mut self, p: Point, geo: &EdgeGeometry<'_>) {
        if !self.shown && geo.in_band(p) {
            self.suppressed = true;
            self.due = None;
        }
    }

    /// The pointer left the surface: nothing to keep the bar for.
    pub fn on_leave(&mut self) -> bool {
        self.path.clear();
        self.suppressed = false;
        self.touch_from = None;
        if self.pinned {
            self.due = None;
            return false;
        }
        self.set_shown(false)
    }

    pub fn on_touch_start(&mut self, p: Point, geo: &EdgeGeometry<'_>) {
        self.touch_from = geo.in_band(p).then_some(p);
    }

    pub fn on_touch_move(&mut self, p: Point) -> bool {
        match self.touch_from {
            Some(from) if p.y - from.y >= TOUCH_SWIPE_PX => {
                self.touch_from = None;
                self.set_shown(true)
            }
            _ => false,
        }
    }

    pub fn on_touch_end(&mut self) {
        self.touch_from = None;
    }

    pub fn settle(&mut self, now: Instant) -> bool {
        if let Some(due) = self.due
            && now >= due
        {
            return self.set_shown(true);
        }
        if let Some(hold) = self.hold
            && now >= hold.until()
            && !self.pinned
        {
            return self.set_shown(false);
        }
        false
    }

    /// When [`Self::settle`] next changes something with no further input,
    /// so the host can sleep until then instead of pumping frames.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.due.into_iter().chain(self.hold.map(Hold::until)).min()
    }

    pub fn force_shown(&mut self, shown: bool) {
        self.path.clear();
        self.pinned = false;
        self.due = None;
        self.hold = None;
        self.shown = shown;
    }

    pub fn elapse(&mut self) -> bool {
        if let Some(deadline) = self.deadline() {
            self.settle(deadline);
        }
        self.path.clear();
        self.shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    /// A maximized window under a 36 px desktop bar; its bar's controls at the
    /// trailing end, below the band.
    const WINDOW: Rect = Rect::new(0.0, 36.0, 1200.0, 800.0);
    const TITLE: Rect = Rect::new(6.0, 42.0, 86.0, 64.0);
    const CLOSE: Rect = Rect::new(1172.0, 42.0, 1194.0, 64.0);
    const RESTORE: Rect = Rect::new(1090.0, 42.0, 1168.0, 64.0);

    fn geo() -> EdgeGeometry<'static> {
        EdgeGeometry {
            window: WINDOW,
            bar: &[TITLE, RESTORE, CLOSE],
            controls: &[RESTORE, CLOSE],
        }
    }

    fn path(
        r: &mut TopEdgeReveal,
        from: Point,
        to: Point,
        steps: u32,
        ms_total: u64,
        t: Instant,
    ) -> Instant {
        let mut at = t;
        for i in 1..=steps {
            let f = f64::from(i) / f64::from(steps);
            at = ms(t, ms_total * u64::from(i) / u64::from(steps));
            r.on_motion(from.lerp(to, f), &geo(), at);
        }
        at
    }

    #[test]
    fn hidden_at_rest_anywhere_in_the_app() {
        let t0 = crate::motion::frame_now();
        let mut r = TopEdgeReveal::default();
        for (i, y) in [400.0, 120.0, 60.0, 43.0].into_iter().enumerate() {
            let at = ms(t0, 1000 * u64::try_from(i).unwrap());
            r.on_motion(Point::new(600.0, y), &geo(), at);
            r.settle(ms(at, 900));
            assert!(!r.shown(), "resting at y={y} points at the app");
        }
        assert_eq!(r.deadline(), None, "nothing to wake for");
    }

    #[test]
    fn a_rest_in_the_band_shows_the_bar_after_the_dwell() {
        let t0 = crate::motion::frame_now();
        let mut r = TopEdgeReveal::default();
        let t = path(
            &mut r,
            Point::new(600.0, 60.0),
            Point::new(600.0, 43.0),
            8,
            400,
            t0,
        );
        let t = ms(t, 50);
        r.on_motion(Point::new(600.0, 38.0), &geo(), t);
        assert!(!r.shown());
        assert_eq!(r.deadline(), Some(t + DWELL));
        r.on_motion(Point::new(640.0, 37.0), &geo(), ms(t, 100));
        assert_eq!(r.deadline(), Some(t + DWELL));
        assert!(!r.settle(ms(t, 149)));
        assert!(r.settle(t + DWELL));
        assert!(r.shown());
    }

    #[test]
    fn pushing_up_to_the_edge_shows_the_bar_at_once() {
        let t0 = crate::motion::frame_now();
        let mut r = TopEdgeReveal::default();
        let t = path(
            &mut r,
            Point::new(900.0, 400.0),
            Point::new(990.0, 40.0),
            6,
            120,
            t0,
        );
        assert!(
            !r.shown(),
            "still flying: it may be bound for the desktop's bar"
        );
        r.on_motion(Point::new(992.0, 38.0), &geo(), ms(t, 12));
        assert!(r.shown(), "a fast climb that brakes in the band is a push");
    }

    #[test]
    fn a_fast_move_that_stops_dead_in_the_band_still_rests_there() {
        let t0 = crate::motion::frame_now();
        let mut r = TopEdgeReveal::default();
        let t = path(
            &mut r,
            Point::new(600.0, 400.0),
            Point::new(600.0, 39.0),
            6,
            120,
            t0,
        );
        assert!(!r.shown());
        assert_eq!(
            r.deadline(),
            Some(t + DWELL),
            "counted from the last sample"
        );
        assert!(r.settle(t + DWELL));
    }

    #[test]
    fn a_fast_pass_through_the_band_is_transit() {
        let t0 = crate::motion::frame_now();
        let mut r = TopEdgeReveal::default();
        let t = path(
            &mut r,
            Point::new(600.0, 400.0),
            Point::new(600.0, 39.0),
            6,
            120,
            t0,
        );
        assert!(!r.shown(), "no push credit while still flying");
        // It flies on into the desktop's bar: the dwell never matures.
        r.on_motion(Point::new(600.0, 0.0), &geo(), ms(t, 13));
        assert_eq!(r.deadline(), None);
        r.settle(ms(t, 1000));
        assert!(!r.shown());
    }

    #[test]
    fn crossing_a_shown_control_in_transit_does_not_hold_the_bar() {
        let t0 = crate::motion::frame_now();
        let mut r = TopEdgeReveal::default();
        r.force_shown(true);
        let x = RESTORE.center().x;
        r.on_motion(Point::new(x, 37.0), &geo(), t0);
        for i in 1..=8u32 {
            let p = Point::new(x, 37.0 + 8.0 * f64::from(i));
            r.on_motion(p, &geo(), ms(t0, 4 * u64::from(i)));
            if p.y > RESTORE.y1 + 4.0 {
                assert!(!r.shown(), "crossing Restore did not hold the bar at {p:?}");
            }
        }
        assert!(!r.shown());
        r.settle(ms(t0, 1000));
        assert!(!r.shown());

        r.force_shown(true);
        for (i, dx) in [-6.0, -4.0, -2.0, 0.0].into_iter().enumerate() {
            let at = ms(t0, 980 + 5 * u64::try_from(i).unwrap());
            r.on_motion(TITLE.center() + (dx, 0.0), &geo(), at);
        }
        for (i, px) in [400.0, 800.0, 1050.0, 1110.0, 1125.0]
            .into_iter()
            .enumerate()
        {
            let at = ms(t0, 1010 + 10 * u64::try_from(i).unwrap());
            r.on_motion(Point::new(px, 53.0), &geo(), at);
            assert!(r.shown(), "on its way to Restore at x={px}");
        }
        r.settle(ms(t0, 5000));
        assert!(r.shown(), "settled on Restore");
    }

    #[test]
    fn aiming_at_the_apps_top_content_never_shows_the_bar() {
        let t0 = crate::motion::frame_now();
        let mut r = TopEdgeReveal::default();
        let t = path(
            &mut r,
            Point::new(300.0, 400.0),
            Point::new(300.0, 52.0),
            6,
            120,
            t0,
        );
        assert!(!r.shown());
        r.settle(ms(t, 2000));
        assert!(!r.shown(), "resting on the app's top content");
        path(
            &mut r,
            Point::new(300.0, 52.0),
            Point::new(1100.0, 50.0),
            10,
            300,
            ms(t, 2000),
        );
        assert!(!r.shown());
    }

    #[test]
    fn coming_down_from_the_desktop_bar_is_not_a_push() {
        let t0 = crate::motion::frame_now();
        let mut r = TopEdgeReveal::default();
        r.on_motion(Point::new(600.0, 10.0), &geo(), t0);
        r.on_motion(Point::new(600.0, 30.0), &geo(), ms(t0, 10));
        r.on_motion(Point::new(600.0, 38.0), &geo(), ms(t0, 20));
        assert!(!r.shown(), "only the dwell can show it");
        assert!(r.deadline().is_some());
        r.on_motion(Point::new(600.0, 90.0), &geo(), ms(t0, 40));
        assert_eq!(r.deadline(), None);
        r.settle(ms(t0, 500));
        assert!(!r.shown());
    }

    #[test]
    fn a_click_in_the_band_goes_to_the_app_and_cancels_the_reveal() {
        let t0 = crate::motion::frame_now();
        let mut r = TopEdgeReveal::default();
        let p = Point::new(200.0, 38.0);
        r.on_motion(p, &geo(), t0);
        r.on_press(p, &geo());
        assert_eq!(r.deadline(), None);
        r.on_motion(Point::new(201.0, 38.0), &geo(), ms(t0, 50));
        r.settle(ms(t0, 1000));
        assert!(!r.shown(), "still in the band after the click");
        r.on_motion(Point::new(201.0, 80.0), &geo(), ms(t0, 1100));
        r.on_motion(Point::new(201.0, 38.0), &geo(), ms(t0, 1600));
        assert!(r.elapse());
    }

    #[test]
    fn the_shown_bar_holds_under_menu_aim_and_hides_otherwise() {
        let t0 = crate::motion::frame_now();
        let mut r = TopEdgeReveal::default();
        r.force_shown(true);
        r.on_motion(TITLE.center(), &geo(), t0);
        let t = path(
            &mut r,
            TITLE.center(),
            Point::new(1060.0, 54.0),
            12,
            360,
            t0,
        );
        assert!(r.shown(), "heading for Restore");
        assert!(r.deadline().is_some());
        r.settle(t + AIM_HOLD);
        assert!(!r.shown());

        r.force_shown(true);
        r.on_motion(Point::new(600.0, 38.0), &geo(), ms(t, 1000));
        r.on_motion(Point::new(600.0, 46.0), &geo(), ms(t, 1020));
        r.on_motion(Point::new(600.0, 56.0), &geo(), ms(t, 1040));
        assert!(!r.shown(), "the pointer went for the app under the bar");
    }

    #[test]
    fn it_never_hides_under_a_pointer_on_a_control_or_on_the_bar() {
        let t0 = crate::motion::frame_now();
        let mut r = TopEdgeReveal::default();
        r.force_shown(true);
        r.on_motion(CLOSE.center(), &geo(), t0);
        r.settle(ms(t0, 10_000));
        assert!(r.shown());
        r.on_motion(TITLE.center(), &geo(), ms(t0, 10_100));
        r.settle(ms(t0, 20_000));
        assert!(r.shown(), "on the title chip");
    }

    #[test]
    fn an_overshoot_into_the_desktop_bar_gets_a_grace_to_return() {
        let t0 = crate::motion::frame_now();
        let mut r = TopEdgeReveal::default();
        r.force_shown(true);
        r.on_motion(Point::new(1100.0, 40.0), &geo(), t0);
        r.on_motion(Point::new(1100.0, 20.0), &geo(), ms(t0, 20));
        assert!(r.shown());
        r.on_motion(RESTORE.center(), &geo(), ms(t0, 200));
        r.settle(ms(t0, 2000));
        assert!(r.shown(), "came back onto Restore");
        r.on_motion(Point::new(1120.0, 10.0), &geo(), ms(t0, 2100));
        r.settle(ms(t0, 2100) + OVERSHOOT_GRACE);
        assert!(!r.shown(), "stayed up there: done with the window");
    }

    #[test]
    fn keyboard_and_touch_reach_the_bar() {
        let t0 = crate::motion::frame_now();
        let mut r = TopEdgeReveal::default();
        assert!(r.pin());
        r.on_motion(Point::new(600.0, 500.0), &geo(), ms(t0, 10));
        assert!(r.shown(), "pinned for the keyboard");
        r.unpin();
        r.on_motion(Point::new(600.0, 520.0), &geo(), ms(t0, 20));
        assert!(!r.shown());

        r.on_touch_start(Point::new(300.0, 37.0), &geo());
        assert!(!r.on_touch_move(Point::new(300.0, 50.0)));
        assert!(r.on_touch_move(Point::new(300.0, 70.0)));
        r.on_touch_start(Point::new(300.0, 300.0), &geo());
        r.hide();
        assert!(!r.on_touch_move(Point::new(300.0, 400.0)));
    }

    #[test]
    fn showing_and_hiding_asks_for_no_frames() {
        let t0 = crate::motion::frame_now();
        let mut r = TopEdgeReveal::default();
        assert!(r.pin());
        assert!(r.shown());
        assert_eq!(crate::motion::frames_wanted_by(t0), Vec::<String>::new());
        assert!(r.hide());
        assert!(!r.shown());
        assert_eq!(crate::motion::frames_wanted_by(t0), Vec::<String>::new());
    }
}
