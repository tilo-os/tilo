//! Pointer intent for chrome that hides along a window's top edge until the
//! person reaches for it: the decorations of a maximized window (its title,
//! Full screen, Restore and close) and, in a Tilo app that puts its header
//! there, the app's header row with them as one bar.
//!
//! A maximized window gives its whole height to the app, so the bar hides at
//! rest and appears *over* the window's top edge. The top of an app is also
//! where the app keeps its own controls (a browser's tabs, an editor's
//! toolbar, a terminal's first line), so the hard part is telling "click
//! something at the top of the app" from "reach the window's bar". The rules,
//! in priority order:
//!
//! 1. **Only the very top asks for the bar.** The bar is summoned from the
//!    reveal band, the top [`REVEAL_BAND_PX`] of the window: above every
//!    control an app puts in its top row (a header row keeps its buttons
//!    `spacing.s3` in; the bar's own controls start below the band too).
//!    Resting on, or aiming at, anything below the band is pointing at the
//!    app and never shows the bar.
//! 2. **Transit is not aim.** Passing over the band, the bar or one of its
//!    controls on the way somewhere else means nothing: a sample is *transit*
//!    while the pointer moves faster than [`TRANSIT_SPEED`] without braking
//!    (its speed still above [`BRAKE_RATIO`] of its recent peak). Only a
//!    pointer that decelerates and settles counts as aiming at what it is on.
//! 3. **Pushing to the edge is intent.** A pointer that brakes in the band
//!    after a fast climb — it left the app's content at least
//!    [`PUSH_TRAVEL_PX`] below within [`PUSH_WINDOW`], on a path steeper than
//!    45° — was not slowing down for anything in the app's top row: it is
//!    aiming at the edge (Fitts; where the window's top is the screen's, the
//!    edge itself stops it), and the bar shows at once.
//! 4. **Otherwise it takes a short rest** in the band, [`DWELL`], counted
//!    from when the pointer settled there. A pointer that came down into the
//!    band from the desktop's top bar is on its way into the app and gets no
//!    push credit.
//! 5. **A click in the band while the bar is hidden belongs to the app.** It
//!    cancels a pending reveal until the pointer leaves the band, so a person
//!    clicking a tab at the very top never gets the bar dropped over it.
//! 6. **While shown, the bar stays** while the pointer settles on it (its chips, or
//!    the whole header row of a Tilo app's merged bar) or in the band, and
//!    while the pointer heads for one of its controls (menu aim: inside the
//!    hull from a recent position to the control, re-checked on every sample,
//!    lapsing [`AIM_HOLD`] after the last confirming one) — so crossing the
//!    app's top row from the title to Close keeps it, while stopping on the
//!    app's content between them hides it, and so does crossing the bar in
//!    transit. A
//!    pointer that overshoots up into the desktop's top bar gets
//!    [`OVERSHOOT_GRACE`] to come back. Anything else hides it at once.
//! 7. **Aimed-at controls don't move.** Showing and hiding only fade the
//!    bar in place; nothing it covers or uncovers shifts, and its controls sit
//!    below the band, so the reveal never puts a control under a resting
//!    pointer. The bar never hides while the pointer has settled on (or is
//!    heading for) one of its controls; a pointer merely crossing one does
//!    not hold it.
//! 8. **Keyboard and touch reach it too.** [`TopEdgeReveal::pin`] shows it for
//!    keyboard use until [`TopEdgeReveal::unpin`]; a touch that starts in the
//!    band and drags down [`TOUCH_SWIPE_PX`] shows it.
//!
//! The desktop's own top bar may hide too. Both share this one model so they
//! never fight over the edge: when both are hidden, approaching the top
//! reveals the top stack together — the desktop's bar and the window's bar
//! under it — the way a person reads them, top to bottom. While the
//! desktop's bar shows over the window's top edge, the window's bar stands
//! below it (`WindowGroup::set_top_overlay_inset`), never under it.
//!
//! Transit, braking and heading are read from the system's one pointer
//! predictor, [`super::aim::Trajectory`], the same reading the tab row and
//! the desk widgets use.
//!
//! The state is pure: every method takes the instant it reasons about, so
//! fast and slow pointer paths are tested against a synthetic timeline.

use super::aim::{Trajectory, in_aim_hull};
use crate::time::{Duration, Instant};
use vello::kurbo::{Point, Rect};

/// Height of the band at the window's top that summons the bar, in logical
/// px. Thinner than the inset any header row keeps above its controls.
pub const REVEAL_BAND_PX: f64 = crate::tokens::Metrics::BASE.icon_label_gap as f64;

/// How long the pointer rests in the band before the bar shows. A little
/// longer than a hover-open dwell: what lies under the band is the app's.
pub const DWELL: Duration = Duration::from_millis(150);

/// How far below the band a pushing pointer started, at most
/// [`PUSH_WINDOW`] ago.
pub const PUSH_TRAVEL_PX: f64 = crate::tokens::Spacing::BASE.s12 as f64;

/// How recent the start of a push must be.
pub const PUSH_WINDOW: Duration = Duration::from_millis(160);

/// How long a shown bar waits for a pointer that stopped heading for one of
/// its controls without arriving. The same hold the tab row uses.
pub const AIM_HOLD: Duration = super::hover_intent::AIM_HOLD;

/// How long a shown bar waits for a pointer that overshot into the desktop's
/// top bar above it.
pub const OVERSHOOT_GRACE: Duration = Duration::from_millis(300);

/// Pointer speed (logical px per ms) above which a sample that is not
/// braking is transit: on its way somewhere else.
pub const TRANSIT_SPEED: f64 = 0.6;

/// A pointer is braking while its speed has fallen to this share of its peak
/// over the last few samples.
pub const BRAKE_RATIO: f64 = 0.6;

/// Peak speed a climb must reach to count as a push for the edge.
pub const PUSH_SPEED: f64 = crate::tokens::motion::PUSH_SPEED;

/// How far a touch that starts in the band drags down to show the bar.
pub const TOUCH_SWIPE_PX: f64 = crate::tokens::Spacing::BASE.s6 as f64;

/// Where the bar is, as laid out now.
#[derive(Clone, Copy, Debug)]
pub struct EdgeGeometry<'a> {
    /// The window whose top edge the bar stands on.
    pub window: Rect,
    /// What the shown bar paints over the window (its chips, or a whole
    /// header row): a pointer on any of them keeps the bar.
    pub bar: &'a [Rect],
    /// The bar's clickable controls, where they stand (or will stand).
    pub controls: &'a [Rect],
}

impl EdgeGeometry<'_> {
    /// The reveal band at the top of the window.
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

    /// Above the window, within its span: the desktop's top bar.
    fn above(&self, p: Point) -> bool {
        p.y < self.window.y0 && p.x >= self.window.x0 && p.x < self.window.x1
    }
}

/// Why a shown bar waits before hiding while the pointer is off it.
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

/// Reveal state of one top-edge bar. See the module docs.
#[derive(Clone, Debug, Default)]
pub struct TopEdgeReveal {
    shown: bool,
    /// The bar shows at this instant unless the pointer says otherwise.
    due: Option<Instant>,
    hold: Option<Hold>,
    pinned: bool,
    /// A click in the band went to the app: no reveal until the pointer
    /// leaves the band.
    suppressed: bool,
    path: Trajectory,
    touch_from: Option<Point>,
    /// When `shown` last changed.
    changed_at: Option<Instant>,
    /// The bar fading in and out with it (`motion::CROSS_FADE`).
    fade: crate::motion::Presence,
}

impl TopEdgeReveal {
    /// Whether the bar shows (and its controls take the pointer).
    #[must_use]
    pub fn shown(&self) -> bool {
        self.shown
    }

    /// When `shown` last changed.
    #[must_use]
    pub fn changed_at(&self) -> Option<Instant> {
        self.changed_at
    }

    /// How far the bar has faded in at `now`, `0..=1`: it cross-fades in and
    /// out with the standard cross-fade, and under reduced motion it is
    /// there or not.
    #[must_use]
    pub fn opacity(&self, now: Instant, motion: crate::tokens::EffectiveMotion) -> f64 {
        if motion.reduced() || self.changed_at.is_none() {
            return if self.shown { 1.0 } else { 0.0 };
        }
        self.fade.progress(now)
    }

    /// Whether the fade is still running at `now`.
    #[must_use]
    pub fn fading(&self, now: Instant, motion: crate::tokens::EffectiveMotion) -> bool {
        !motion.reduced() && self.changed_at.is_some() && self.fade.is_moving(now)
    }

    /// Show or hide; returns whether that changed anything.
    fn set_shown(&mut self, shown: bool, now: Instant) -> bool {
        self.due = None;
        self.hold = None;
        if self.shown == shown {
            return false;
        }
        self.shown = shown;
        self.changed_at = Some(now);
        self.fade.show(
            shown,
            crate::motion::CROSS_FADE,
            crate::tokens::EffectiveMotion::default(),
            crate::tokens::MotionSource::Pointer,
            now,
        );
        true
    }

    /// Forget everything and hide at once (the window stopped being
    /// maximized, or the bar's geometry is gone).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Hide now, e.g. after one of the bar's controls was used.
    pub fn hide(&mut self, now: Instant) -> bool {
        self.pinned = false;
        self.set_shown(false, now)
    }

    /// Show the bar for keyboard use until [`Self::unpin`].
    pub fn pin(&mut self, now: Instant) -> bool {
        self.pinned = true;
        self.set_shown(true, now)
    }

    /// Keyboard use ended; the pointer decides again on its next sample.
    pub fn unpin(&mut self) {
        self.pinned = false;
    }

    /// A pointer sample. Returns whether `shown` changed.
    pub fn on_motion(&mut self, p: Point, geo: &EdgeGeometry<'_>, now: Instant) -> bool {
        let mut changed = self.settle(now);
        // A repeated position (the host and the window both report the same
        // sample) says nothing new.
        if self.path.last().is_some_and(|(last, _)| last == p) {
            return changed;
        }
        // Recent positions the path came from: a cone from any of them
        // toward a control counts, so neither one jittery sample nor the
        // lag of an older apex after a turn misreads the heading.
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
            // On the bar the pointer may yet settle (a fast flick onto Close
            // ends with a fast sample and then silence); one passing over it
            // is off it by the next sample, which decides.
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
            changed |= self.set_shown(false, now);
            return changed;
        }
        if !in_band || self.suppressed {
            self.due = None;
            return changed;
        }
        if self.pushed_up(p, geo, now) {
            changed |= self.set_shown(true, now);
        } else if transit {
            // Sweeping through the band: the rest counts from when it
            // settles. A pointer that stops sends no further samples, so the
            // dwell restarts at every fast sample and matures only once the
            // samples stop or slow.
            self.due = Some(now + DWELL);
        } else if self.due.is_none() {
            self.due = Some(now + DWELL);
        }
        changed
    }

    /// Whether the pointer at `p` (in the band) arrived pushing up from the
    /// app's content and brakes at the edge, rather than slowing down on
    /// something in its top row or passing on into the desktop's bar.
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

    /// A press. A press in the band while the bar hides goes to the app and
    /// cancels any reveal until the pointer leaves the band.
    pub fn on_press(&mut self, p: Point, geo: &EdgeGeometry<'_>) {
        if !self.shown && geo.in_band(p) {
            self.suppressed = true;
            self.due = None;
        }
    }

    /// The pointer left the surface: nothing to keep the bar for.
    pub fn on_leave(&mut self, now: Instant) -> bool {
        self.path.clear();
        self.suppressed = false;
        self.touch_from = None;
        if self.pinned {
            self.due = None;
            return false;
        }
        self.set_shown(false, now)
    }

    /// A touch went down at `p`.
    pub fn on_touch_start(&mut self, p: Point, geo: &EdgeGeometry<'_>) {
        self.touch_from = geo.in_band(p).then_some(p);
    }

    /// A touch moved to `p`. Returns whether `shown` changed.
    pub fn on_touch_move(&mut self, p: Point, now: Instant) -> bool {
        match self.touch_from {
            Some(from) if p.y - from.y >= TOUCH_SWIPE_PX => {
                self.touch_from = None;
                self.set_shown(true, now)
            }
            _ => false,
        }
    }

    /// A touch lifted or was cancelled.
    pub fn on_touch_end(&mut self) {
        self.touch_from = None;
    }

    /// Advance timers to `now` without a sample. Returns whether `shown`
    /// changed.
    pub fn settle(&mut self, now: Instant) -> bool {
        if let Some(due) = self.due
            && now >= due
        {
            return self.set_shown(true, now);
        }
        if let Some(hold) = self.hold
            && now >= hold.until()
            && !self.pinned
        {
            return self.set_shown(false, now);
        }
        false
    }

    /// When [`Self::settle`] next changes something with no further input,
    /// so the host can sleep until then instead of pumping frames.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.due.into_iter().chain(self.hold.map(Hold::until)).min()
    }

    /// Test and shot fixture: as if the person had reached for the bar.
    pub fn force_shown(&mut self, shown: bool) {
        // Time has passed, as far as the pointer's path is concerned.
        self.path.clear();
        self.pinned = false;
        self.due = None;
        self.hold = None;
        self.shown = shown;
        self.changed_at = None;
        self.fade.snap(shown);
    }

    /// Test and shot clock control: settle as if the pending dwell or hold
    /// had run out. Returns whether the bar shows now.
    pub fn elapse(&mut self) -> bool {
        if let Some(deadline) = self.deadline() {
            self.settle(deadline);
        }
        // The pointer rested that long: its next move starts from rest, and
        // the fade (timed from a deadline that has not come yet on the real
        // clock) has run.
        self.path.clear();
        self.changed_at = None;
        self.fade.snap(self.shown);
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

    /// Drive a straight path from `from` to `to` in `steps` samples spread
    /// over `ms_total`, starting at `t`. Returns the instant of the last one.
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
        // Slow, careful approach from just below: no push credit.
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
        // Moving within the band keeps the dwell counted from entry.
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
        // It brakes at the edge.
        r.on_motion(Point::new(992.0, 38.0), &geo(), ms(t, 12));
        assert!(r.shown(), "a fast climb that brakes in the band is a push");
    }

    #[test]
    fn a_fast_move_that_stops_dead_in_the_band_still_rests_there() {
        let t0 = crate::motion::frame_now();
        let mut r = TopEdgeReveal::default();
        // Fast all the way, then silence: the last sample was fast, but the
        // pointer is resting in the band.
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
        // Straight up through the band into the desktop's bar at full speed.
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
        // From the band straight down through Restore into the app at a
        // steady, fast pace: transit over the control, not aim.
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

        // A pointer flying from the title toward Restore and braking onto it
        // is aiming at it: it stays.
        r.force_shown(true);
        // Resting on the title first, so the path's recent history is there.
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
        // A quick flick up that stops on the app's top row (a tab at y=52),
        // just below the band, and rests there.
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
        // A sideways sweep along the top row doesn't either.
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
        // ...and passing through into the app cancels the dwell.
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
        // Leaving and coming back arms it again.
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
        // From the title chip across the app's top row toward Restore: off
        // every chip on the way, but heading for a control the whole time.
        let t = path(
            &mut r,
            TITLE.center(),
            Point::new(1060.0, 54.0),
            12,
            360,
            t0,
        );
        assert!(r.shown(), "heading for Restore");
        // Stops short on the app's content: the hold lapses.
        assert!(r.deadline().is_some());
        r.settle(t + AIM_HOLD);
        assert!(!r.shown());

        // Shown again from the band; moving down onto the app's top row,
        // away from every chip, hides it at once.
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
        assert!(r.pin(t0));
        r.on_motion(Point::new(600.0, 500.0), &geo(), ms(t0, 10));
        assert!(r.shown(), "pinned for the keyboard");
        r.unpin();
        r.on_motion(Point::new(600.0, 520.0), &geo(), ms(t0, 20));
        assert!(!r.shown());

        r.on_touch_start(Point::new(300.0, 37.0), &geo());
        assert!(!r.on_touch_move(Point::new(300.0, 50.0), ms(t0, 30)));
        assert!(r.on_touch_move(Point::new(300.0, 70.0), ms(t0, 40)));
        r.on_touch_start(Point::new(300.0, 300.0), &geo());
        r.hide(ms(t0, 50));
        assert!(!r.on_touch_move(Point::new(300.0, 400.0), ms(t0, 60)));
    }

    #[test]
    fn the_fade_runs_in_place_and_reports_its_end() {
        let t0 = crate::motion::frame_now();
        let mut r = TopEdgeReveal::default();
        r.pin(t0);
        let motion = crate::tokens::EffectiveMotion::default();
        let mid = r.opacity(ms(t0, 40), motion);
        assert!(mid > 0.0 && mid < 1.0, "{mid}");
        assert!(r.fading(ms(t0, 40), motion));
        assert!((r.opacity(ms(t0, 200), motion) - 1.0).abs() < 1e-9);
        assert!(!r.fading(ms(t0, 200), motion));
        let reduced = crate::tokens::EffectiveMotion::new(crate::tokens::Motion::default(), true);
        assert!((r.opacity(t0, reduced) - 1.0).abs() < 1e-9);
    }
}
