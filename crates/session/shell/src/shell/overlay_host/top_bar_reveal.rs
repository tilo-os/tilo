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

use std::time::Instant;

use tilo_desktop::chrome::TopBarPresence;
use ui::widget::top_edge_reveal::{DWELL, EdgeGeometry, TopEdgeReveal};
use vello::kurbo::{Point, Rect};

/// The display's top row of pixels. The pointer cannot travel past it, so a
/// path that reaches it has stopped there, however fast it arrived; the
/// edge model, which reads a stop from slowing samples, never sees one.
const EDGE_ROW_PX: f64 = 1.0;

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
        // Along the bar it stays.
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
        // A click right at the top goes to the window and cancels the rest.
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
}
