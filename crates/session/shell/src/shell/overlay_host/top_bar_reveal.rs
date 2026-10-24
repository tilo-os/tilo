//! When the top bar the person chose to hide shows.
//!
//! "Hide the top bar until the pointer is near it" (`desktop_controls.
//! hide_top_bar`) gives the bar's strip to the windows. The bar comes back
//! over them, in its docked place, when the person reaches for it, and the
//! reaching is read by the same top-edge model as a maximized window's bar
//! ([`ui::widget::top_edge_reveal`]), so the two never fight over the edge:
//!
//! - **The pointer pushes to the top edge, or rests there.** A path still
//!   travelling up into the top band is a push and shows it at once; one
//!   that slows down on something near the top (an app's tab row) is the
//!   app's, and only a short rest in the band shows the bar. Passing across
//!   the band on the way elsewhere is transit, not a request.
//! - **While shown it stays** while the pointer is on it, heads for it
//!   (menu aim), or one of its menus is open (Quick Settings, the people
//!   card, workspace settings, the clock card, the widget editor); anything
//!   else hides it.
//!
//! A press in the band while the bar hides belongs to the window under it
//! and cancels the reveal until the pointer leaves the band.

use std::time::{Duration, Instant};

use tilo_desktop::chrome::TopBarPresence;
use ui::widget::top_edge_reveal::{DWELL, EdgeGeometry, TopEdgeReveal};
use vello::kurbo::{Point, Rect};

/// The display's top row of pixels. The pointer cannot travel past it, so a
/// path that reaches it has stopped there, however fast it arrived; the
/// edge model, which reads a stop from slowing samples, never sees one.
const EDGE_ROW_PX: f64 = ui::tokens::Metrics::BASE.chrome_rule_px as f64;

#[derive(Debug, Default)]
pub(crate) struct TopBarReveal {
    /// The person's choice; when off the bar is docked and nothing here runs.
    hide: bool,
    edge: TopEdgeReveal,
    /// The pointer stopped against the display's top edge at this instant
    /// plus the rest it takes to show the bar.
    edge_due: Option<Instant>,
    /// A press against the top edge went to the window there: no reveal
    /// until the pointer leaves the edge.
    edge_click: bool,
}

impl TopBarReveal {
    /// Follow the person's setting. Turning it off docks the bar.
    pub(crate) fn set_hide(&mut self, hide: bool) {
        if self.hide != hide {
            self.hide = hide;
            self.edge.reset();
            self.edge_due = None;
            self.edge_click = false;
        }
    }

    /// What the bar is now, given whether one of its menus holds it open.
    pub(crate) fn presence(&self, held_open: bool) -> TopBarPresence {
        if !self.hide {
            TopBarPresence::Docked
        } else if self.edge.shown() || held_open {
            TopBarPresence::Shown
        } else {
            TopBarPresence::Hidden
        }
    }

    /// The geometry the edge model reads: the bar stands on `display`'s
    /// top edge and occupies `bar`.
    fn geometry(display: Rect, bar: &[Rect]) -> EdgeGeometry<'_> {
        EdgeGeometry {
            window: display,
            bar,
            controls: bar,
        }
    }

    /// A pointer sample at `p`. Returns whether the presence may have
    /// changed.
    pub(crate) fn on_motion(&mut self, p: Point, display: Rect, bar: Rect, now: Instant) -> bool {
        if !self.hide {
            return false;
        }
        // The whole strip is the bar: on it, a shown bar stays however fast
        // the pointer crosses it (from the clock to the status, say). The
        // edge model reads quick passes over a window's few chips as transit.
        if self.edge.shown() && bar.contains(p) {
            return false;
        }
        let bars = [bar];
        let changed = self.edge.on_motion(p, &Self::geometry(display, &bars), now);
        if !Self::at_edge(p, display) {
            self.edge_due = None;
            self.edge_click = false;
        } else if !self.edge.shown() && !self.edge_click && self.edge_due.is_none() {
            self.edge_due = now.checked_add(DWELL);
        }
        changed
    }

    fn at_edge(p: Point, display: Rect) -> bool {
        p.y < display.y0 + EDGE_ROW_PX && p.x >= display.x0 && p.x < display.x1
    }

    /// A press at `p`: one in the band while the bar hides is the window's.
    pub(crate) fn on_press(&mut self, p: Point, display: Rect, bar: Rect) {
        if self.hide {
            let bars = [bar];
            self.edge.on_press(p, &Self::geometry(display, &bars));
            if !self.edge.shown() && Self::at_edge(p, display) {
                self.edge_click = true;
                self.edge_due = None;
            }
        }
    }

    /// Advance the timers to `now`. Returns whether the presence may have
    /// changed.
    pub(crate) fn settle(&mut self, now: Instant) -> bool {
        if !self.hide {
            return false;
        }
        let mut changed = self.edge.settle(now);
        if let Some(due) = self.edge_due
            && now >= due
        {
            self.edge_due = None;
            if !self.edge.shown() {
                // Shown as by a rest in the band; the next pointer sample
                // decides again whether it stays.
                self.edge.pin(now);
                self.edge.unpin();
                changed = true;
            }
        }
        changed
    }

    /// When [`Self::settle`] next has something to do.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        if !self.hide {
            return None;
        }
        self.edge.deadline().into_iter().chain(self.edge_due).min()
    }

    /// A finger swiped down from the display's top edge: show the hidden
    /// bar, as a push against the edge does. Returns whether it changed.
    pub(crate) fn reveal_by_touch(&mut self, now: Instant) -> bool {
        if !self.hide {
            return false;
        }
        self.edge_due = None;
        self.edge_click = false;
        self.edge.reveal_by_touch(now)
    }

    /// Whether the bar is hidden now: the person hides it and nothing
    /// shows it.
    pub(crate) fn hidden(&self) -> bool {
        self.hide && !self.edge.shown()
    }
}

/// How long a fullscreen window's bar stays after it is shown without the
/// pointer on it: entering fullscreen, a key or a swipe from the top edge
/// shows it, and it goes once this passes with nothing holding it.
pub(crate) const FULLSCREEN_BAR_IDLE: Duration = Duration::from_secs(2);

/// A fullscreen window's bar, which hides with the rest of the desktop's
/// chrome. It shows as the window enters fullscreen, so the person sees
/// where it went, and collapses after [`FULLSCREEN_BAR_IDLE`]. Reaching for
/// the top edge brings it back, by the same edge model as the top bar the
/// person hides, and so do a finger swiping down from the edge and a key
/// (Super, which never reaches the app). It shows over the window, which
/// keeps its whole size, so nothing moves as it comes and goes.
///
/// Shown by the pointer, it stays while the pointer is on it and goes when
/// the pointer goes elsewhere. Shown by entering, a key or a swipe, it stays
/// for the idle time whatever the pointer does over the window, and longer
/// while the pointer rests on it or the bar is in use (its location field,
/// its search, a tab being dragged).
#[derive(Debug, Default)]
pub(crate) struct FullscreenBarReveal {
    /// The fullscreen window, whose bar this is.
    group: Option<u64>,
    edge: TopEdgeReveal,
    /// Shown without the pointer: it collapses at this instant unless
    /// something holds it.
    linger: Option<Instant>,
    /// The pointer's last sample was on the bar.
    on_bar: bool,
}

impl FullscreenBarReveal {
    /// Follow the window that is fullscreen now. A window entering
    /// fullscreen shows its bar for the idle time; none hides it.
    pub(crate) fn follow(&mut self, group: Option<u64>, now: Instant) {
        if self.group != group {
            self.group = group;
            self.edge.reset();
            self.linger = None;
            self.on_bar = false;
            if group.is_some() {
                self.show_for_idle(now);
            }
        }
    }

    fn show_for_idle(&mut self, now: Instant) -> bool {
        self.linger = now.checked_add(FULLSCREEN_BAR_IDLE);
        self.edge.pin(now)
    }

    /// Whether the fullscreen window's bar shows.
    pub(crate) fn shown(&self) -> bool {
        self.group.is_some() && self.edge.shown()
    }

    /// The fullscreen window's group.
    pub(crate) fn group(&self) -> Option<u64> {
        self.group
    }

    /// A pointer sample at `p` over the window `display` with its bar at
    /// `bar`. Returns whether it may have changed.
    pub(crate) fn on_motion(&mut self, p: Point, display: Rect, bar: Rect, now: Instant) -> bool {
        if self.group.is_none() {
            return false;
        }
        self.on_bar = bar.contains(p);
        if self.edge.shown() && self.on_bar {
            return false;
        }
        let bars = [bar];
        self.edge
            .on_motion(p, &TopBarReveal::geometry(display, &bars), now)
    }

    /// A press at `p`: one in the band while the bar hides is the window's.
    pub(crate) fn on_press(&mut self, p: Point, display: Rect, bar: Rect) {
        if self.group.is_some() {
            let bars = [bar];
            self.edge
                .on_press(p, &TopBarReveal::geometry(display, &bars));
        }
    }

    /// A finger swiped down from the top edge: show the bar for the idle
    /// time, as no pointer rests on it to keep it.
    pub(crate) fn reveal_by_touch(&mut self, now: Instant) -> bool {
        self.on_bar = false;
        self.group.is_some() && self.show_for_idle(now)
    }

    /// The key that asks for the bar went down: show it for the idle time,
    /// or keep it that long again.
    pub(crate) fn reveal_by_key(&mut self, now: Instant) -> bool {
        self.group.is_some() && self.show_for_idle(now)
    }

    /// Advance the timers to `now`; `in_use` says the bar is being used
    /// (which holds it). Returns whether it may have changed.
    pub(crate) fn settle(&mut self, now: Instant, in_use: bool) -> bool {
        if self.group.is_none() {
            return false;
        }
        let mut changed = self.edge.settle(now);
        if let Some(due) = self.linger
            && now >= due
        {
            if in_use {
                self.linger = now.checked_add(FULLSCREEN_BAR_IDLE);
            } else {
                self.linger = None;
                self.edge.unpin();
                // On the bar the pointer keeps it, as one that reached for
                // it does; anywhere else it goes now, sample or not.
                if !self.on_bar {
                    changed |= self.edge.hide(now);
                }
            }
        }
        changed
    }

    /// When [`Self::settle`] next has something to do. Nothing while the
    /// bar is hidden and the pointer is away from the edge.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.group?;
        self.edge.deadline().into_iter().chain(self.linger).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const DISPLAY: Rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    const BAR: Rect = Rect::new(0.0, 0.0, 1280.0, 24.0);

    fn ms(t: Instant, n: u64) -> Instant {
        t.checked_add(Duration::from_millis(n))
            .expect("test instants stay in range")
    }

    /// A straight path from `from` to `to` in `steps` samples over `total`
    /// milliseconds from `t`; returns the last sample's instant.
    fn path(
        r: &mut TopBarReveal,
        from: Point,
        to: Point,
        steps: u32,
        total: u64,
        t: Instant,
    ) -> Instant {
        let mut at = t;
        for i in 1..=steps {
            let f = f64::from(i) / f64::from(steps);
            at = ms(
                t,
                total
                    .saturating_mul(u64::from(i))
                    .checked_div(u64::from(steps))
                    .unwrap_or(total),
            );
            r.on_motion(from.lerp(to, f), DISPLAY, BAR, at);
        }
        at
    }

    #[test]
    fn docked_unless_the_person_hides_it() {
        let mut r = TopBarReveal::default();
        let t0 = Instant::now();
        path(
            &mut r,
            Point::new(600.0, 500.0),
            Point::new(600.0, 0.0),
            6,
            100,
            t0,
        );
        assert_eq!(r.presence(false), TopBarPresence::Docked);
        assert_eq!(r.deadline(), None);
        r.set_hide(true);
        assert_eq!(r.presence(false), TopBarPresence::Hidden);
        assert_eq!(r.presence(true), TopBarPresence::Shown, "a menu holds it");
    }

    #[test]
    fn a_push_to_the_top_edge_shows_it_and_leaving_hides_it() {
        let mut r = TopBarReveal::default();
        r.set_hide(true);
        let t0 = Instant::now();
        let t = path(
            &mut r,
            Point::new(600.0, 500.0),
            Point::new(620.0, 0.0),
            6,
            120,
            t0,
        );
        // Stopped against the edge, it shows after the rest it takes; a
        // click there before then would have been the window's.
        assert_eq!(r.deadline(), Some(t + DWELL));
        assert!(r.settle(t + DWELL));
        assert_eq!(
            r.presence(false),
            TopBarPresence::Shown,
            "pushed to the edge"
        );
        let t = t + DWELL;
        let t = path(
            &mut r,
            Point::new(620.0, 0.0),
            Point::new(1200.0, 12.0),
            8,
            400,
            t,
        );
        assert_eq!(r.presence(false), TopBarPresence::Shown);
        // Straight down into the windows, away from it: gone.
        path(
            &mut r,
            Point::new(1200.0, 12.0),
            Point::new(700.0, 400.0),
            6,
            120,
            t,
        );
        r.settle(ms(t, 2000));
        assert_eq!(r.presence(false), TopBarPresence::Hidden);
    }

    #[test]
    fn slowing_onto_a_window_near_the_top_is_not_reaching_for_the_bar() {
        let mut r = TopBarReveal::default();
        r.set_hide(true);
        let t0 = Instant::now();
        // A flick up that settles on a tab row 14 px down, below the band.
        let t = path(
            &mut r,
            Point::new(300.0, 500.0),
            Point::new(300.0, 14.0),
            6,
            120,
            t0,
        );
        r.settle(ms(t, 2000));
        assert_eq!(r.presence(false), TopBarPresence::Hidden);
        let p = Point::new(310.0, 2.0);
        r.on_motion(p, DISPLAY, BAR, ms(t, 2100));
        r.on_press(p, DISPLAY, BAR);
        r.settle(ms(t, 5000));
        assert_eq!(r.presence(false), TopBarPresence::Hidden);
        // So does one flung against the very edge and clicked at once (a
        // browser's tabs run to the top of a maximized window).
        let t = path(
            &mut r,
            Point::new(310.0, 300.0),
            Point::new(330.0, 0.0),
            4,
            60,
            ms(t, 6000),
        );
        r.on_press(Point::new(330.0, 0.0), DISPLAY, BAR);
        assert_eq!(r.deadline(), None, "the rest is cancelled");
        r.settle(ms(t, 2000));
        assert_eq!(r.presence(false), TopBarPresence::Hidden);
    }

    #[test]
    fn a_fullscreen_windows_bar_shows_on_entering_and_collapses_when_idle() {
        let mut r = FullscreenBarReveal::default();
        let t0 = Instant::now();
        r.follow(Some(7), t0);
        assert!(r.shown(), "entering fullscreen shows where the bar went");
        // The pointer moving over the window does not cut it short.
        r.on_motion(Point::new(600.0, 500.0), DISPLAY, BAR, ms(t0, 100));
        r.on_motion(Point::new(640.0, 520.0), DISPLAY, BAR, ms(t0, 140));
        assert!(r.shown());
        let due = r.deadline().expect("it is due to collapse");
        assert_eq!(due, t0 + FULLSCREEN_BAR_IDLE);
        assert!(r.settle(due, false));
        assert!(!r.shown(), "idle, it collapses");
        assert_eq!(r.deadline(), None, "and nothing wakes for it");
        // The same window again is nothing new; leaving fullscreen forgets it.
        r.follow(Some(7), ms(t0, 5000));
        assert!(!r.shown());
        r.follow(None, ms(t0, 5000));
        assert_eq!(r.deadline(), None);
    }

    #[test]
    fn a_key_or_a_swipe_brings_it_back_for_the_idle_time() {
        let mut r = FullscreenBarReveal::default();
        let t0 = Instant::now();
        r.follow(Some(7), t0);
        r.settle(t0 + FULLSCREEN_BAR_IDLE, false);
        assert!(!r.shown());
        let t = ms(t0, 3000);
        assert!(r.reveal_by_key(t), "the key shows it");
        assert_eq!(r.deadline(), Some(t + FULLSCREEN_BAR_IDLE));
        // Pressed again, it stays that long again.
        let again = ms(t, 1500);
        assert!(!r.reveal_by_key(again));
        assert!(!r.settle(t + FULLSCREEN_BAR_IDLE, false));
        assert!(r.shown());
        r.settle(again + FULLSCREEN_BAR_IDLE, false);
        assert!(!r.shown());
        let t = ms(t, 9000);
        assert!(r.reveal_by_touch(t), "a swipe from the top edge shows it");
        r.settle(t + FULLSCREEN_BAR_IDLE, false);
        assert!(
            !r.shown(),
            "and, no finger resting on it, it goes when idle"
        );
    }

    #[test]
    fn the_pointer_on_it_or_using_it_keeps_it() {
        let mut r = FullscreenBarReveal::default();
        let t0 = Instant::now();
        r.follow(Some(7), t0);
        // In use (its location field, say), it stays past the idle time.
        let due = t0 + FULLSCREEN_BAR_IDLE;
        r.settle(due, true);
        assert!(r.shown(), "in use, it stays");
        assert_eq!(r.deadline(), Some(due + FULLSCREEN_BAR_IDLE));
        // Resting on it, it stays; leaving it for the window hides it.
        r.on_motion(Point::new(600.0, 12.0), DISPLAY, BAR, ms(t0, 2500));
        r.settle(due + FULLSCREEN_BAR_IDLE, false);
        assert!(r.shown(), "the pointer on it keeps it");
        let t = ms(t0, 7000);
        for (i, y) in [60.0, 200.0, 400.0].into_iter().enumerate() {
            let at = ms(t, 40 * (u64::try_from(i).unwrap_or(0) + 1));
            r.on_motion(Point::new(600.0, y), DISPLAY, BAR, at);
        }
        r.settle(ms(t, 2000), false);
        assert!(!r.shown(), "off it, into the window: gone");
    }
}
