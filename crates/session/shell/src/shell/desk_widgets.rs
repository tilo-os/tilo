//! Desk widgets: the things the person keeps around their work (what is
//! playing, the people in this workspace, the date, how busy the machine
//! is), and when each shows itself.
//!
//! A widget is in one of three forms:
//!
//! - **Away** while the person types into a window, and whenever it has
//!   nothing to show. Typing is the one signal of "working in something
//!   else": a key delivered to the focused window.
//! - **Glance**: a tiny, static form at its edge. A widget comes back to it
//!   when the person pauses typing for [`GLANCE_AFTER_PAUSE`], or reaches for
//!   its edge: the pointer heading there and coming to rest
//!   ([`ui::widget::EdgeAim`]). Passing over an edge on the way elsewhere is
//!   transit, and brings nothing.
//! - **Open**: the full card, only on purpose: resting on the glance form
//!   for [`OPEN_DWELL`], clicking it, or the Super surface.
//!
//! Each widget's [`ShowMode`] narrows this: "only when I ask" never comes
//! back on a pause, "always show on this strip" stays in its glance form
//! there even while the person types.
//!
//! Two rules hold over all of it: a widget never covers the text caret (it
//! stays away rather than move), and a widget never appears over a pointer
//! that has settled where it would appear: the person is aiming at
//! whatever is under it.
//!
//! [`DeskBoard`] is pure: every method takes the instant it reasons about,
//! so typing, pauses and pointer paths are tested against a synthetic
//! timeline. The overlay host feeds it and paints its decisions
//! (`overlay_host::desk`).

pub mod calendar;
pub mod layout;
pub mod system_monitor;
pub mod team;

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use vello::kurbo::{Point, Rect};

pub use settings_store::desk_widgets::{
    DeskWidgetPrefs, DockEdge, ShowMode, WidgetId, WidgetPrefs,
};

/// How long the person must stop typing before widgets come back to their
/// glance form.
///
/// Keystroke-logging research reads a gap of two seconds or more as a
/// planning pause; the gaps inside a word or between words are well under a
/// second. Three seconds is past that threshold, so hesitating over the next
/// word or clause never makes the edges flicker, and still soon enough that
/// sitting back to read what you wrote brings the widgets.
pub const GLANCE_AFTER_PAUSE: Duration = Duration::from_secs(3);

/// How long the pointer rests on a glance form before it opens: a
/// deliberate rest, longer than a pointer crossing the form spends on it.
pub const OPEN_DWELL: Duration = Duration::from_millis(350);

/// How long a widget brought by reaching for its edge stays once the
/// pointer has left its reach zone without coming back.
pub const REACH_HOLD: Duration = Duration::from_millis(600);

/// How long an open card waits after the pointer leaves it before it folds
/// back, so a hand that overshoots its edge by a few pixels does not lose it.
pub const LEAVE_GRACE: Duration = Duration::from_millis(300);

/// How a widget comes and goes is the standard ambient motion
/// ([`ui::motion::Ambient`]): it leaves at once (the person started typing,
/// covered the desk or let go of Super, and something leaving has nothing to
/// say), arrives unasked slowly and opacity only, and arrives asked for
/// (reaching for its edge, Super, a click) quickly.

/// The space kept clear around the text caret, so a widget near it never
/// sits on the line being typed.
pub const CARET_CLEARANCE: f64 = ui::tokens::Spacing::BASE.s6 as f64;

/// One of a widget's three forms.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Form {
    #[default]
    Away,
    Glance,
    Open,
}

/// One widget as laid out this frame, in desktop coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    pub id: WidgetId,
    /// Where its glance form sits.
    pub glance: Rect,
    /// Where its open card sits.
    pub open: Rect,
    /// Where a pointer reaching for it comes to rest: the glance form and
    /// the strip of edge behind it.
    pub reach: Rect,
    /// Whether it has anything to show (a player, someone else here).
    pub has_content: bool,
}

/// Everything besides the board's own history that decides the forms.
#[derive(Clone, Copy, Debug)]
pub struct Context<'a> {
    pub now: Instant,
    pub prefs: &'a DeskWidgetPrefs,
    /// The active strip, for [`ShowMode::Always`].
    pub strip: Option<u64>,
    /// The focused field's caret, in desktop coordinates.
    pub caret: Option<Rect>,
    /// The whole Super surface is up: every widget with content is open.
    pub super_surface: bool,
    /// The Super surface's quiet first stage (Super held, not yet grown):
    /// every widget with content in its glance form, whatever the person
    /// was doing.
    pub super_glance: bool,
    /// Another surface owns the desk (Quick Settings, the overview, plain
    /// search, a sheet): widgets stay away under it.
    pub covered: bool,
    pub slots: &'a [Slot],
}

/// Typing, pointer intent and the forms they decide.
#[derive(Clone, Debug, Default)]
pub struct DeskBoard {
    aim: ui::widget::EdgeAim,
    pointer: Option<Point>,
    last_typed: Option<Instant>,
    /// The widget the pointer reached for, and when that was last confirmed.
    reached: Option<(WidgetId, Instant)>,
    /// The pointer is still in the reach zone of the widget it reached for:
    /// the reach holds without a timer until it moves.
    in_reach: bool,
    /// The widget opened on purpose.
    opened: Option<WidgetId>,
    /// When the pointer left the opened card.
    left_open: Option<Instant>,
    /// The glance form the pointer is resting on, since when.
    rest: Option<(WidgetId, Instant)>,
    forms: BTreeMap<WidgetId, Form>,
    /// Widgets that would have appeared under a settled pointer and wait
    /// for it to leave.
    held: BTreeSet<WidgetId>,
}

impl DeskBoard {
    /// The form `id` was last decided in.
    #[must_use]
    pub fn form(&self, id: WidgetId) -> Form {
        self.forms.get(&id).copied().unwrap_or_default()
    }

    /// Whether the person is typing at `now`: a key reached the focused
    /// window less than [`GLANCE_AFTER_PAUSE`] ago.
    #[must_use]
    pub fn typing(&self, now: Instant) -> bool {
        self.typing_until().is_some_and(|until| now < until)
    }

    /// When the current typing run counts as a pause.
    #[must_use]
    pub fn typing_until(&self) -> Option<Instant> {
        self.last_typed
            .and_then(|at| at.checked_add(GLANCE_AFTER_PAUSE))
    }

    /// The last key delivered to the focused window.
    #[must_use]
    pub fn last_typed(&self) -> Option<Instant> {
        self.last_typed
    }

    /// A key reached the focused window at `at`. Widgets go away (an open
    /// card the pointer is not on folds too); reaching for an edge is
    /// forgotten.
    pub fn note_typed(&mut self, at: Instant, slots: &[Slot]) {
        if self.last_typed.is_some_and(|last| last >= at) {
            return;
        }
        self.last_typed = Some(at);
        self.reached = None;
        self.rest = None;
        let on_open = self.opened.is_some_and(|id| {
            self.pointer.is_some_and(|p| {
                slots
                    .iter()
                    .any(|slot| slot.id == id && slot.open.contains(p))
            })
        });
        if !on_open {
            self.opened = None;
            self.left_open = None;
        }
    }

    /// A pointer sample at `p`.
    pub fn on_pointer(&mut self, p: Point, now: Instant, slots: &[Slot]) {
        self.pointer = Some(p);
        let reachable: Vec<&Slot> = slots.iter().filter(|slot| slot.has_content).collect();
        let zones: Vec<Rect> = reachable.iter().map(|slot| slot.reach).collect();
        if let Some(index) = self.aim.on_motion(p, now, &zones)
            && let Some(slot) = reachable.get(index)
        {
            self.reached = Some((slot.id, now));
        }
        // Staying in reach (or on the form it brought) keeps it.
        self.hold_reach(slots, now);
        // Resting on a glance form opens it after the dwell.
        let resting = slots
            .iter()
            .find(|slot| self.form(slot.id) == Form::Glance && slot.glance.contains(p))
            .map(|slot| slot.id);
        self.rest = match (resting, self.rest) {
            (Some(id), Some((was, since))) if id == was => Some((id, since)),
            (Some(id), _) => Some((id, now)),
            (None, _) => None,
        };
        // An open card stays while the pointer is on it or on its glance.
        if let Some(id) = self.opened
            && let Some(slot) = slots.iter().find(|slot| slot.id == id)
        {
            let on = slot.open.inflate(8.0, 8.0).contains(p) || slot.glance.contains(p);
            if on {
                self.left_open = None;
            } else if self.left_open.is_none() {
                self.left_open = Some(now);
            }
        }
    }

    /// Refresh the reach while the pointer stays in the reached widget's
    /// zone or on the form it brought.
    fn hold_reach(&mut self, slots: &[Slot], now: Instant) {
        self.in_reach = match (self.reached, self.pointer) {
            (Some((id, _)), Some(p)) => slots.iter().any(|slot| {
                slot.id == id
                    && (slot.reach.contains(p) || slot.glance.inflate(8.0, 8.0).contains(p))
            }),
            _ => false,
        };
        if self.in_reach
            && let Some((id, _)) = self.reached
        {
            self.reached = Some((id, now));
        }
    }

    /// The pointer left the display.
    pub fn pointer_left(&mut self, now: Instant) {
        self.pointer = None;
        self.in_reach = false;
        self.aim.clear();
        self.rest = None;
        if self.opened.is_some() && self.left_open.is_none() {
            self.left_open = Some(now);
        }
    }

    /// Open `id` on purpose (a click on its glance form, an action).
    pub fn open(&mut self, id: WidgetId) {
        self.opened = Some(id);
        self.left_open = None;
        self.rest = None;
    }

    /// Fold whatever is open back to its glance form.
    pub fn close(&mut self) {
        self.opened = None;
        self.left_open = None;
    }

    /// The widget open on purpose, if any.
    #[must_use]
    pub fn opened(&self) -> Option<WidgetId> {
        self.opened
    }

    /// The widget the pointer reached for, while that reach holds.
    #[must_use]
    pub fn reached(&self) -> Option<WidgetId> {
        self.reached.map(|(id, _)| id)
    }

    /// Advance the timers to `ctx.now` and decide every widget's form.
    /// Returns whether any form changed.
    pub fn update(&mut self, ctx: &Context<'_>) -> bool {
        let now = ctx.now;
        // A pointer thrown at an edge stops there with no slow sample; once
        // it has been still a moment, that rest is the reach.
        let reachable: Vec<&Slot> = ctx.slots.iter().filter(|slot| slot.has_content).collect();
        let zones: Vec<Rect> = reachable.iter().map(|slot| slot.reach).collect();
        if let Some(slot) = self
            .aim
            .rested_in(now, &zones)
            .and_then(|index| reachable.get(index))
            && self.reached.is_none_or(|(id, _)| id != slot.id)
            && self.form(slot.id) == Form::Away
        {
            self.reached = Some((slot.id, now));
        }
        self.hold_reach(ctx.slots, now);
        if self
            .reached
            .is_some_and(|(_, at)| now.saturating_duration_since(at) >= REACH_HOLD)
        {
            self.reached = None;
        }
        if self
            .left_open
            .is_some_and(|at| now.saturating_duration_since(at) >= LEAVE_GRACE)
        {
            self.opened = None;
            self.left_open = None;
        }
        if let Some((id, since)) = self.rest
            && now.saturating_duration_since(since) >= OPEN_DWELL
            && self.aim.settled(now)
            && !self.typing(now)
        {
            self.open(id);
        }
        let mut changed = false;
        for slot in ctx.slots {
            let before = self.form(slot.id);
            let mut form = self.decide(slot, ctx);
            // Never appear under a pointer resting there: it is aiming at
            // what the widget would cover. Reaching for the widget itself
            // is the exception, and the Super surface is asked for.
            let settled_under = self.pointer.is_some_and(|p| {
                slot.glance.inflate(4.0, 4.0).contains(p) && self.aim.settled(now)
            });
            let reached = self.reached.is_some_and(|(id, _)| id == slot.id);
            if before == Form::Away
                && form != Form::Away
                && !ctx.super_surface
                && !ctx.super_glance
                && !reached
                && (settled_under || self.held.contains(&slot.id))
            {
                if settled_under {
                    self.held.insert(slot.id);
                } else {
                    self.held.remove(&slot.id);
                }
                form = if settled_under { Form::Away } else { form };
            } else if !settled_under {
                self.held.remove(&slot.id);
            }
            if form != before {
                changed = true;
                self.forms.insert(slot.id, form);
            }
        }
        changed
    }

    fn decide(&self, slot: &Slot, ctx: &Context<'_>) -> Form {
        let prefs = ctx.prefs.get(slot.id);
        if !prefs.enabled || !slot.has_content {
            return Form::Away;
        }
        if ctx.super_surface {
            return Form::Open;
        }
        if ctx.super_glance {
            return if self.opened == Some(slot.id) {
                Form::Open
            } else {
                Form::Glance
            };
        }
        if ctx.covered {
            return Form::Away;
        }
        if self.opened == Some(slot.id) {
            return Form::Open;
        }
        if ctx.caret.is_some_and(|caret| {
            caret
                .inflate(CARET_CLEARANCE, CARET_CLEARANCE)
                .overlaps(slot.glance)
        }) {
            return Form::Away;
        }
        if self.reached.is_some_and(|(id, _)| id == slot.id) {
            return Form::Glance;
        }
        match prefs.show {
            ShowMode::Ask => Form::Away,
            ShowMode::Always if prefs.always_on(ctx.strip) => Form::Glance,
            ShowMode::Always | ShowMode::Pause => {
                if self.typing(ctx.now) {
                    Form::Away
                } else {
                    Form::Glance
                }
            }
        }
    }

    /// When the board next changes with no further input, so the loop can
    /// sleep until then instead of pumping frames.
    #[must_use]
    pub fn deadline(&self, now: Instant) -> Option<Instant> {
        let pause = self.typing_until().filter(|at| *at > now);
        let reach = self
            .reached
            .filter(|_| !self.in_reach)
            .and_then(|(_, at)| at.checked_add(REACH_HOLD));
        let dwell = self.rest.and_then(|(_, at)| at.checked_add(OPEN_DWELL));
        let leave = self.left_open.and_then(|at| at.checked_add(LEAVE_GRACE));
        // Settling only matters while something waits on it: a dwell, a
        // held widget, or a pointer that may be resting in a reach zone.
        let settle = (self.rest.is_some() || !self.held.is_empty() || self.pointer.is_some())
            .then(|| self.aim.deadline(now))
            .flatten();
        [pause, reach, dwell, leave, settle]
            .into_iter()
            .flatten()
            .filter(|at| *at > now)
            .min()
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "test timelines add milliseconds to one monotonic instant"
)]
mod tests {
    use super::*;

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    /// Music in the bottom-right corner, Team on the right edge.
    fn slots() -> Vec<Slot> {
        vec![
            Slot {
                id: WidgetId::Music,
                glance: Rect::new(1356.0, 852.0, 1588.0, 888.0),
                open: Rect::new(1268.0, 740.0, 1588.0, 888.0),
                reach: Rect::new(1332.0, 828.0, 1600.0, 900.0),
                has_content: true,
            },
            Slot {
                id: WidgetId::Team,
                glance: Rect::new(1552.0, 400.0, 1588.0, 500.0),
                open: Rect::new(1268.0, 330.0, 1588.0, 570.0),
                reach: Rect::new(1528.0, 376.0, 1600.0, 524.0),
                has_content: true,
            },
        ]
    }

    struct Rig {
        board: DeskBoard,
        prefs: DeskWidgetPrefs,
        slots: Vec<Slot>,
        caret: Option<Rect>,
        super_surface: bool,
        super_glance: bool,
        strip: Option<u64>,
    }

    impl Rig {
        fn new() -> Self {
            Self {
                board: DeskBoard::default(),
                prefs: DeskWidgetPrefs::default(),
                slots: slots(),
                caret: None,
                super_surface: false,
                super_glance: false,
                strip: Some(1),
            }
        }

        fn at(&mut self, now: Instant) -> (Form, Form) {
            let ctx = Context {
                now,
                prefs: &self.prefs,
                strip: self.strip,
                caret: self.caret,
                super_surface: self.super_surface,
                super_glance: self.super_glance,
                covered: false,
                slots: &self.slots,
            };
            self.board.update(&ctx);
            (
                self.board.form(WidgetId::Music),
                self.board.form(WidgetId::Team),
            )
        }

        fn typed(&mut self, at: Instant) {
            self.board.note_typed(at, &self.slots);
        }

        fn pointer(&mut self, p: (f64, f64), at: Instant) {
            self.board.on_pointer(Point::new(p.0, p.1), at, &self.slots);
        }
    }

    #[test]
    fn typing_sends_widgets_away_and_a_pause_brings_their_glance_back() {
        let t0 = Instant::now();
        let mut rig = Rig::new();
        assert_eq!(
            rig.at(t0),
            (Form::Glance, Form::Glance),
            "idle desk glances"
        );
        // A burst of typing with sub-second gaps: away the whole time.
        for k in 0..20 {
            rig.typed(ms(t0, 100 + k * 180));
            assert_eq!(rig.at(ms(t0, 100 + k * 180)), (Form::Away, Form::Away));
        }
        let last = ms(t0, 100 + 19 * 180);
        // A two-second hesitation is still typing.
        assert_eq!(
            rig.at(last + Duration::from_secs(2)),
            (Form::Away, Form::Away)
        );
        assert_eq!(
            rig.board.deadline(last + Duration::from_secs(2)),
            Some(last + GLANCE_AFTER_PAUSE),
            "the loop sleeps until the pause, not frame by frame"
        );
        assert_eq!(
            rig.at(last + GLANCE_AFTER_PAUSE),
            (Form::Glance, Form::Glance)
        );
    }

    #[test]
    fn passing_over_an_edge_does_not_glance_but_reaching_for_it_does() {
        let t0 = Instant::now();
        let mut rig = Rig::new();
        rig.typed(t0);
        rig.at(t0);
        // Down the right edge at speed, across Team's zone.
        for k in 0..30 {
            let at = ms(t0, 50 + k * 8);
            rig.pointer(
                (1590.0, 100.0 + 16.0 * f64::from(u32::try_from(k).unwrap())),
                at,
            );
            assert_eq!(rig.at(at).1, Form::Away, "transit at sample {k}");
        }
        // Then a reach into the bottom-right corner, decelerating.
        let mut rig = Rig::new();
        rig.typed(t0);
        rig.at(t0);
        for (k, p) in [(1000.0, 560.0), (1160.0, 656.0), (1280.0, 728.0)]
            .into_iter()
            .enumerate()
        {
            rig.pointer(p, ms(t0, 400 + u64::try_from(k).unwrap() * 8));
        }
        let at = ms(t0, 420);
        assert!(rig.board.typing(at), "still inside the typing run");
        assert_eq!(
            rig.at(at),
            (Form::Glance, Form::Away),
            "only the reached widget"
        );
        // It holds while the pointer stays in reach, then lets go.
        rig.pointer((1500.0, 880.0), ms(t0, 500));
        rig.pointer((800.0, 400.0), ms(t0, 520));
        assert_eq!(rig.at(ms(t0, 499) + REACH_HOLD).0, Form::Glance);
        assert_eq!(rig.at(ms(t0, 500) + REACH_HOLD).0, Form::Away);
    }

    #[test]
    fn a_reach_holds_while_the_pointer_rests_there_with_no_timer() {
        let t0 = Instant::now();
        let mut rig = Rig::new();
        rig.typed(t0);
        rig.at(t0);
        // Thrown into the corner and left there: no more motion arrives.
        for (k, p) in [(1300.0, 700.0), (1450.0, 790.0), (1599.0, 899.0)]
            .into_iter()
            .enumerate()
        {
            rig.pointer(p, ms(t0, 100 + u64::try_from(k).unwrap() * 8));
        }
        let rest = ms(t0, 116);
        assert_eq!(rig.at(rest + Duration::from_millis(80)).0, Form::Glance);
        assert_eq!(rig.at(ms(t0, 2500)).0, Form::Glance, "still resting there");
        assert_eq!(
            rig.board.deadline(ms(t0, 2500)),
            Some(t0 + GLANCE_AFTER_PAUSE),
            "nothing wakes for the reach while the pointer rests in it"
        );
    }

    #[test]
    fn resting_on_a_glance_opens_it_and_leaving_folds_it() {
        let t0 = Instant::now();
        let mut rig = Rig::new();
        assert_eq!(rig.at(t0).0, Form::Glance);
        // Into the music pill and still.
        rig.pointer((1300.0, 860.0), t0);
        rig.pointer((1400.0, 868.0), ms(t0, 8));
        rig.pointer((1401.0, 868.0), ms(t0, 40));
        assert_eq!(
            rig.at(ms(t0, 200)).0,
            Form::Glance,
            "a short rest is not a request"
        );
        assert_eq!(rig.at(ms(t0, 8) + OPEN_DWELL).0, Form::Open);
        // Off the card toward the middle: folds after the grace.
        let away = ms(t0, 800);
        rig.pointer((700.0, 300.0), away);
        assert_eq!(rig.at(away + Duration::from_millis(100)).0, Form::Open);
        assert_eq!(rig.at(away + LEAVE_GRACE).0, Form::Glance);
    }

    #[test]
    fn crossing_a_glance_form_in_transit_does_not_open_it() {
        let t0 = Instant::now();
        let mut rig = Rig::new();
        rig.at(t0);
        for k in 0..20u32 {
            let at = ms(t0, u64::from(k) * 8);
            rig.pointer((1580.0 - 30.0 * f64::from(k), 870.0), at);
            assert_ne!(rig.at(at).0, Form::Open);
        }
        assert_ne!(
            rig.at(ms(t0, 2000)).0,
            Form::Open,
            "it left; nothing to open"
        );
    }

    #[test]
    fn each_show_mode_is_honoured() {
        let t0 = Instant::now();
        let mut rig = Rig::new();
        rig.prefs.music.show = ShowMode::Ask;
        rig.prefs.team.show = ShowMode::Always;
        rig.prefs.team.always_on_strips = vec![1];
        assert_eq!(
            rig.at(t0),
            (Form::Away, Form::Glance),
            "ask stays away at rest"
        );
        rig.typed(ms(t0, 10));
        assert_eq!(
            rig.at(ms(t0, 10)),
            (Form::Away, Form::Glance),
            "always holds through typing on its strip"
        );
        rig.strip = Some(2);
        assert_eq!(
            rig.at(ms(t0, 20)).1,
            Form::Away,
            "elsewhere it is a pause widget"
        );
        assert_eq!(
            rig.at(ms(t0, 10) + GLANCE_AFTER_PAUSE),
            (Form::Away, Form::Glance),
            "a pause never brings an ask-only widget"
        );
        rig.super_surface = true;
        assert_eq!(
            rig.at(ms(t0, 5000)),
            (Form::Open, Form::Open),
            "Super asks for all"
        );
        rig.super_surface = false;
        rig.prefs.music.enabled = false;
        assert_eq!(
            rig.at(ms(t0, 6000)).0,
            Form::Away,
            "a removed widget never shows"
        );
    }

    #[test]
    fn a_widget_stays_away_rather_than_cover_the_caret() {
        let t0 = Instant::now();
        let mut rig = Rig::new();
        // A field near the bottom-right corner, caret right under the pill.
        rig.caret = Some(Rect::new(1400.0, 840.0, 1402.0, 858.0));
        assert_eq!(rig.at(t0), (Form::Away, Form::Glance));
        rig.prefs.music.show = ShowMode::Always;
        assert_eq!(
            rig.at(ms(t0, 10)).0,
            Form::Away,
            "not even when always shown"
        );
        rig.caret = Some(Rect::new(400.0, 300.0, 402.0, 318.0));
        assert_eq!(rig.at(ms(t0, 20)).0, Form::Glance);
    }

    #[test]
    fn a_widget_never_appears_under_a_pointer_settled_there() {
        let t0 = Instant::now();
        let mut rig = Rig::new();
        rig.typed(t0);
        rig.at(t0);
        // The pointer rests where the music pill would be, aiming at the
        // app's control underneath; it arrived slowly from inside, not by
        // reaching for the corner.
        rig.pointer((1420.0, 870.0), ms(t0, 100));
        rig.pointer((1420.0, 870.0), ms(t0, 200));
        let pause = t0 + GLANCE_AFTER_PAUSE;
        assert_eq!(
            rig.at(pause).0,
            Form::Away,
            "held while the pointer rests there"
        );
        assert_eq!(rig.at(pause).1, Form::Glance);
        rig.pointer((900.0, 500.0), pause + Duration::from_millis(10));
        rig.pointer((880.0, 490.0), pause + Duration::from_millis(80));
        assert_eq!(rig.at(pause + Duration::from_millis(200)).0, Form::Glance);
    }

    /// Super held, before it grows: the glance forms, even mid-typing and
    /// for "only when I ask" widgets; nothing opens until the surface grows.
    #[test]
    fn the_super_quiet_stage_brings_every_glance_and_opens_none() {
        let t0 = Instant::now();
        let mut rig = Rig::new();
        rig.prefs.music.show = ShowMode::Ask;
        rig.typed(t0);
        assert_eq!(rig.at(t0), (Form::Away, Form::Away));
        rig.super_glance = true;
        assert_eq!(rig.at(ms(t0, 400)), (Form::Glance, Form::Glance));
        rig.super_glance = false;
        rig.super_surface = true;
        assert_eq!(rig.at(ms(t0, 700)), (Form::Open, Form::Open));
        rig.super_surface = false;
        assert_eq!(
            rig.at(ms(t0, 800)),
            (Form::Away, Form::Away),
            "let go mid-typing: back to away"
        );
    }

    #[test]
    fn widgets_leave_at_once_and_arrive_slowly_unless_asked_for() {
        use ui::motion::{Ambient, Presence};
        use ui::tokens::{EffectiveMotion, Motion, MotionSource};
        let d = ui::tokens::Durations::default();
        let motion = EffectiveMotion::new(Motion::default(), false);
        let t0 = ui::time::now();
        let arrive = |asked: bool| {
            let mut p = Presence::closed();
            p.show(
                true,
                Ambient::spec(asked),
                motion,
                MotionSource::Pointer,
                t0,
            );
            p.settles_at().expect("arriving").duration_since(t0)
        };
        let leave = |asked: bool| {
            let mut p = Presence::opened();
            p.show(
                false,
                Ambient::spec(asked),
                motion,
                MotionSource::Pointer,
                t0,
            );
            p.is_visible(t0)
        };
        assert!(!leave(false), "typing: gone");
        assert!(!leave(true));
        let slow = arrive(false);
        assert!(
            slow >= d.slow() / 2 && slow <= d.slow() + Duration::from_millis(1),
            "a pause: slow, {slow:?}"
        );
        let quick = arrive(true);
        assert!(
            quick <= d.fast() + Duration::from_millis(1),
            "reached for: quick, {quick:?}"
        );
    }
}
