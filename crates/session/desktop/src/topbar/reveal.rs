//! When a bar shows over the window that covers its display.
//!
//! A window taken full screen gives its whole display to the app, so the
//! desktop's bar gets out of the way: it takes no room and paints nothing.
//! It comes back *over* the window, in its docked place, when the person
//! reaches for it, so nothing moves as it comes and goes. The reaching is
//! read by the same top-edge model as the decorations of a maximized window
//! ([`ui::widget::top_edge_reveal`]):
//!
//! - **The pointer pushes to the top edge, or rests there.** A path still
//!   travelling up into the top band is a push and shows the bar at once; a
//!   pointer that stops against the display's top row shows it after a short
//!   rest. Passing across the band on the way elsewhere is transit, not a
//!   request.
//! - **Entering full screen shows it for [`FULLSCREEN_BAR_IDLE`]**, so the
//!   person sees where it went, and it goes again once that passes with
//!   nothing holding it. A key (Super, which never reaches the app) and a
//!   finger swiped down from the top edge show it for that time too.
//! - **While shown it stays** while the pointer is on it, heads for it, or
//!   something of it is open (its menus, its own keyboard focus); anything
//!   else hides it.
//!
//! A press in the band while the bar hides belongs to the window under it
//! and cancels the reveal until the pointer leaves the edge. The person's
//! own "hide the top bar until the pointer is near it" setting
//! (`desktop_controls.hide_top_bar`) is the same machinery without a
//! fullscreen window, and shows the bar for no idle time once reached for:
//! it stays while the pointer is on it.

use std::time::Duration;

use ui::time::Instant;
use ui::widget::top_edge_reveal::{
    DWELL, EdgeGeometry, TOUCH_EDGE_PX, TOUCH_SWIPE_PX, TopEdgeReveal,
};
use vello::kurbo::{Point, Rect};

use crate::chrome::TopBarPresence;

/// The display's top row of pixels. The pointer cannot travel past it, so a path that reaches it
/// has stopped there, however fast it arrived; the edge model, which reads a stop from slowing
/// samples, never sees one.
const EDGE_ROW_PX: f64 = ui::tokens::Metrics::BASE.chrome_rule_px as f64;

/// How long a fullscreen window's bar stays after it is shown without the pointer on it: entering
/// full screen, a key or a swipe from the top edge shows it, and it goes once this passes with
/// nothing holding it.
pub const FULLSCREEN_BAR_IDLE: Duration = Duration::from_secs(2);

/// How a finger on the screen moved, as the bar's reveal reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finger {
    /// It touched down.
    Down,
    /// It moved.
    Move,
    /// It left the screen.
    Lift,
    /// The system took it.
    Cancel,
}

/// What the bar on one display does: whether a window covers the display,
/// whether the person reaches for the bar there, and what holds it.
#[derive(Debug, Default)]
struct BarReveal {
    /// A window covers the display, so the bar shows only when reached for.
    fullscreen: bool,
    /// The pointer's reach and rest at the top edge.
    edge: TopEdgeReveal,
    /// The pointer stopped against the display's top edge at this instant plus the rest it takes to
    /// show the bar.
    edge_due: Option<Instant>,
    /// A press against the top edge went to the window there: no reveal until the pointer leaves
    /// the edge.
    edge_click: bool,
    /// Shown without the pointer: it collapses at this instant unless something holds it.
    linger: Option<Instant>,
    /// The pointer's last sample was on the bar.
    on_bar: bool,
    /// When a window entered full screen here, while one is: what the hint saying how to leave it
    /// follows.
    entered: Option<Instant>,
    /// The display's whole rectangle and where the bar stands on it.
    display: Rect,
    bar: Rect,
}

impl BarReveal {
    /// Whether the bar shows over the window.
    fn shown(&self) -> bool {
        self.edge.shown()
    }

    /// Forget the reveal and hide at once.
    fn forget(&mut self) {
        self.edge.reset();
        self.edge_due = None;
        self.edge_click = false;
        self.linger = None;
        self.on_bar = false;
    }

    /// A window covering this display came or went; `now` is when it did.
    fn follow(&mut self, fullscreen: bool, now: Instant) {
        if self.fullscreen == fullscreen {
            return;
        }
        self.fullscreen = fullscreen;
        self.forget();
        self.entered = fullscreen.then_some(now);
        if fullscreen {
            self.show_for_idle(now);
        }
    }

    /// Show it for [`FULLSCREEN_BAR_IDLE`], whatever the pointer does over the window, and keep it
    /// that long again each time.
    fn show_for_idle(&mut self, now: Instant) -> bool {
        self.linger = now.checked_add(FULLSCREEN_BAR_IDLE);
        self.edge.pin()
    }

    /// Whether the bar hides here now: the display is covered or the person hides it, and nothing
    /// shows it.
    fn hidden(&self, hide: bool) -> bool {
        (self.fullscreen || hide) && !self.edge.shown()
    }

    /// A pointer sample at `p` . Returns whether the presence may have changed.
    fn on_motion(&mut self, p: Point, hide: bool, now: Instant) -> bool {
        if !(self.fullscreen || hide) {
            return false;
        }
        // The whole strip is the bar: on it, a shown bar stays however fast the pointer crosses it
        // (from the clock to the status, say).
        if self.edge.shown() && self.bar.contains(p) {
            self.on_bar = true;
            return false;
        }
        self.on_bar = false;
        let (display, bar) = (self.display, self.bar);
        let bars = [bar];
        let changed = self.edge.on_motion(
            p,
            &EdgeGeometry {
                window: display,
                bar: &bars,
                controls: &bars,
            },
            now,
        );
        if !self.at_edge(p) {
            self.edge_due = None;
            self.edge_click = false;
        } else if !self.edge.shown() && !self.edge_click && self.edge_due.is_none() {
            self.edge_due = now.checked_add(DWELL);
        }
        changed
    }

    /// The pointer left the display: a bar it reached for goes.
    fn on_leave(&mut self) -> bool {
        self.on_bar = false;
        self.edge.on_leave()
    }

    fn at_edge(&self, p: Point) -> bool {
        p.y < self.display.y0 + EDGE_ROW_PX && p.x >= self.display.x0 && p.x < self.display.x1
    }

    /// A press at `p` : one in the band while the bar hides is the window's.
    fn on_press(&mut self, p: Point, hide: bool) {
        if !(self.fullscreen || hide) {
            return;
        }
        let (display, bar) = (self.display, self.bar);
        let bars = [bar];
        self.edge.on_press(
            p,
            &EdgeGeometry {
                window: display,
                bar: &bars,
                controls: &bars,
            },
        );
        if !self.edge.shown() && self.at_edge(p) {
            self.edge_click = true;
            self.edge_due = None;
        }
    }

    /// Advance the timers to `now` ; `in_use` says the bar on this display is being used, which
    /// holds it. Returns whether the presence may have changed.
    fn settle(&mut self, now: Instant, hide: bool, in_use: bool) -> bool {
        if !(self.fullscreen || hide) {
            return false;
        }
        let mut changed = self.edge.settle(now);
        if let Some(due) = self.edge_due
            && now >= due
        {
            self.edge_due = None;
            if !self.edge.shown() {
                // Shown as by a rest in the band; the next pointer sample decides again whether it
                // stays.
                self.edge.pin();
                self.edge.unpin();
                changed = true;
            }
        }
        if let Some(due) = self.linger
            && now >= due
        {
            if in_use {
                self.linger = now.checked_add(FULLSCREEN_BAR_IDLE);
            } else {
                self.linger = None;
                self.edge.unpin();
                // On the bar the pointer keeps it, as one that reached for it does; anywhere else
                // it goes now, sample or not.
                if !self.on_bar {
                    changed |= self.edge.hide();
                }
            }
        }
        changed
    }

    /// When [`Self::settle`] next has something to do.
    fn deadline(&self, hide: bool) -> Option<Instant> {
        if !(self.fullscreen || hide) {
            return None;
        }
        self.edge
            .deadline()
            .into_iter()
            .chain(self.edge_due)
            .chain(self.linger)
            .min()
    }

    /// A finger swiped down from the display's top edge: show what hides there, as a push against
    /// the edge does.
    fn reveal_by_touch(&mut self, now: Instant) -> bool {
        self.edge_due = None;
        self.edge_click = false;
        if self.fullscreen {
            self.show_for_idle(now)
        } else {
            self.edge.reveal_by_touch()
        }
    }
}

/// Every display's bar, and when each shows over the window that covers it. One instance for the
/// session; the desk's band and the chrome read it.
#[derive(Debug, Default)]
pub struct Reveals {
    /// The person's choice: the bar stays out of the way until reached for.
    hide: bool,
    /// One per display, the display in use first: `None` names it, as a full bar stands there, and
    /// a compact bar names its own display.
    bars: Vec<(Option<u32>, BarReveal)>,
    /// How tall the bar stands, which the edge model reads it by.
    height: f64,
    /// A finger swiped down from a display's top edge, and where it began.
    swipe: Option<(Option<u32>, Point)>,
}

impl Reveals {
    /// Follow the person's setting. Turning it off docks the bars.
    pub fn set_hide(&mut self, hide: bool) {
        if self.hide != hide {
            self.hide = hide;
            self.swipe = None;
            for (_, bar) in &mut self.bars {
                bar.forget();
            }
        }
    }

    /// How tall the bar stands on its display, which is what it takes of the pointer.
    pub fn set_height(&mut self, height: f64) {
        if self.height.total_cmp(&height) != std::cmp::Ordering::Equal {
            self.height = height;
            for (_, bar) in &mut self.bars {
                bar.bar = bar_on(bar.display, height);
            }
        }
    }

    /// Place each display's bar: which displays a window covers, where each display is and whether
    /// the bar there shows. A window entering or leaving a display shows or forgets that display's
    /// bar at `now` .
    pub fn follow(&mut self, places: &[(Option<u32>, Rect, bool)], now: Instant) {
        let mut bars = Vec::with_capacity(places.len());
        for &(display, rect, fullscreen) in places {
            let mut bar = match self.bars.iter().position(|(key, _)| *key == display) {
                Some(at) => self.bars.remove(at).1,
                None => BarReveal::default(),
            };
            bar.display = rect;
            bar.bar = bar_on(rect, self.height);
            bar.follow(fullscreen, now);
            bars.push((display, bar));
        }
        self.bars = bars;
        if self
            .swipe
            .as_ref()
            .is_some_and(|(display, _)| !self.bars.iter().any(|(key, _)| key == display))
        {
            self.swipe = None;
        }
    }

    /// What the bar on `display` is now, given whether something of it holds it open.
    pub fn presence(&self, display: Option<u32>, held_open: bool) -> TopBarPresence {
        let Some((_, bar)) = self.bars.iter().find(|(key, _)| *key == display) else {
            return TopBarPresence::Docked;
        };
        if !(bar.fullscreen || self.hide) {
            return TopBarPresence::Docked;
        }
        if bar.edge.shown() || held_open {
            TopBarPresence::Shown
        } else {
            TopBarPresence::Hidden
        }
    }

    /// Whether a window covers `display` , so the bar there is out of the way until it is asked
    /// for.
    pub fn covers_display(&self, display: Option<u32>) -> bool {
        self.bars
            .iter()
            .any(|(key, bar)| *key == display && bar.fullscreen)
    }

    /// Whether the bar on `display` shows.
    pub fn shown(&self, display: Option<u32>) -> bool {
        self.bars
            .iter()
            .any(|(key, bar)| *key == display && bar.shown())
    }

    /// The display a window took full screen most recently, and when, while one has: what the hint
    /// saying how to leave it follows.
    pub fn entered(&self) -> Option<(Rect, Instant)> {
        self.bars
            .iter()
            .filter_map(|(_, bar)| bar.entered.map(|at| (bar.display, at)))
            .max_by_key(|(_, at)| *at)
    }

    /// A pointer sample at `p` . Returns whether a bar's presence may have changed.
    pub fn motion(&mut self, p: Point, now: Instant) -> bool {
        let hide = self.hide;
        let mut changed = false;
        for (_, bar) in &mut self.bars {
            if bar.display.contains(p) {
                changed |= bar.on_motion(p, hide, now);
            } else if bar.shown() {
                changed |= bar.on_leave();
            }
        }
        changed
    }

    /// A press at `p` , which is the window's while the bar there hides.
    pub fn press(&mut self, p: Point) {
        let hide = self.hide;
        for (_, bar) in &mut self.bars {
            if bar.display.contains(p) {
                bar.on_press(p, hide);
            }
        }
    }

    /// A finger at `p` . Returns whether the desk takes the contact, so nothing under it sees it:
    /// the contact belongs to the desk when it begins in a display's top edge while the bar there
    /// hides, and shows the bar as it is swiped down.
    pub fn touch(&mut self, finger: Finger, p: Point, now: Instant) -> bool {
        if finger == Finger::Down {
            if self.swipe.is_some() {
                return false;
            }
            let hide = self.hide;
            let Some((key, _)) = self
                .bars
                .iter()
                .find(|(_, bar)| bar.hidden(hide) && in_touch_edge(bar.display, p))
            else {
                return false;
            };
            self.swipe = Some((*key, p));
            return true;
        }
        let Some((key, from)) = self.swipe else {
            return false;
        };
        match finger {
            Finger::Move => {
                if p.y - from.y >= TOUCH_SWIPE_PX {
                    self.swipe = None;
                    if let Some((_, bar)) = self.bars.iter_mut().find(|(held, _)| *held == key) {
                        bar.reveal_by_touch(now);
                    }
                }
                true
            }
            Finger::Lift | Finger::Cancel => {
                self.swipe = None;
                true
            }
            Finger::Down => false,
        }
    }

    /// The key that asks for the bar went down: show the fullscreen window's bar on the display in
    /// use for the idle time.
    pub fn reveal_by_key(&mut self, now: Instant) -> bool {
        let Some((_, bar)) = self.bars.iter_mut().find(|(key, _)| key.is_none()) else {
            return false;
        };
        bar.fullscreen && bar.show_for_idle(now)
    }

    /// Advance every bar's timers to `now` ; `in_use` says whether the bar on that display is being
    /// used, which holds it. Returns whether any presence may have changed.
    pub fn settle(&mut self, now: Instant, in_use: impl Fn(Option<u32>) -> bool) -> bool {
        let hide = self.hide;
        let mut changed = false;
        for (key, bar) in &mut self.bars {
            changed |= bar.settle(now, hide, in_use(*key));
        }
        changed
    }

    /// When [`Self::settle`] next has something to do.
    pub fn deadline(&self) -> Option<Instant> {
        let hide = self.hide;
        self.bars
            .iter()
            .filter_map(|(_, bar)| bar.deadline(hide))
            .min()
    }
}

/// Where the bar stands on a display: its top edge, as tall as it is.
fn bar_on(display: Rect, height: f64) -> Rect {
    Rect::new(
        display.x0,
        display.y0,
        display.x1,
        (display.y0 + height).min(display.y1),
    )
}

/// Whether `p` is in the band at `display` 's top edge a finger swipes down from: wider than the
/// pointer's band, since a finger coming off the bezel lands a few pixels in.
fn in_touch_edge(display: Rect, p: Point) -> bool {
    p.x >= display.x0 && p.x < display.x1 && p.y >= display.y0 && p.y < display.y0 + TOUCH_EDGE_PX
}
