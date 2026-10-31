//! Desk widgets: the things the person keeps around their work (the people in
//! this workspace, the date), and when each shows itself.
//!
//! A widget is in one of three forms:
//!
//! - **Away**: hidden, holding nothing (no press lands on it).
//! - **Glance**: its small resting form, wherever the person put it.
//! - **Open**: its card, expanded in place from the glance.
//!
//! # When a widget expands, and when it goes
//!
//! One rule, with nothing hidden:
//!
//! - **Open only on purpose.** A tap on a glance opens that widget's card in
//!   place. Hovering, resting or reaching never opens anything.
//! - **Closed only on purpose.** A tap anywhere outside the open card, or its
//!   collapse control, folds it back to its glance. Nothing folds it on a
//!   timer.
//! - **Hidden by what the person is visibly doing.** Another surface over the
//!   desk hides them while it is up. A widget that would appear under a
//!   pointer resting there waits for it to leave, so it never takes a click
//!   aimed at what is beneath.
//! - **Never under the hand.** While the pointer is on a widget, or a press on
//!   it is held, that widget keeps its form: nothing above flips it until the
//!   hand leaves or lets go.
//!
//! Each widget's [`ShowMode`] narrows when it comes back: "only when I ask"
//! stays away, "always show on this strip" stays in its glance form.
//!
//! [`DeskBoard`] is pure: every method takes the instant it reasons about, so
//! pointer paths are tested against a synthetic timeline. [`DeskWidgets`] is
//! the layer the desk draws: it feeds the board and paints its decisions
//! ([`paint`]); where each widget rests is [`placement`], what it shows
//! [`calendar`] and [`team`].

pub mod calendar;
pub mod paint;
pub mod placement;
pub mod sizing;
pub mod team;
mod widgets;

pub use widgets::DeskWidgets;

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use ui::desk_widgets::{DeskWidgetPrefs, ShowMode, WidgetId};
use vello::kurbo::{Point, Rect};

/// How long the person must stop typing before widgets come back to their
/// glance form.
///
/// Keystroke-logging research reads a gap of two seconds or more as a
/// planning pause; the gaps inside a word or between words are well under a
/// second. Three seconds is past that threshold, so hesitating over the next
/// word or clause never makes the edges flicker, and still soon enough that
/// sitting back to read what you wrote brings the widgets.
pub const GLANCE_AFTER_PAUSE: Duration = Duration::from_secs(3);

/// How long a widget brought by reaching for it stays once the pointer has
/// left its reach zone without coming back.
pub const REACH_HOLD: Duration = Duration::from_millis(600);

// How a widget comes and goes is the standard ambient motion
// (`ui::motion::Ambient`): it leaves at once (the person started typing,
// covered the desk or let go of Super, and something leaving has nothing to
// say), arrives unasked slowly and opacity only, and arrives asked for
// (reaching for it, Super, a tap) quickly.

/// The space kept clear around the text caret by transient readouts.
pub const CARET_CLEARANCE: f64 = ui::tokens::Spacing::BASE.s6 as f64;

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
    /// Its resting form.
    pub glance: Rect,
    /// Its open card.
    pub open: Rect,
    /// Where a pointer reaching for it comes to rest
    /// ([`placement::reach_zone`]).
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
    /// The whole Super surface is up: every widget with content is open.
    pub super_surface: bool,
    /// The Super surface's quiet first stage (Super held, not yet grown):
    /// every widget with content in its glance form, whatever the person
    /// was doing.
    pub super_glance: bool,
    /// Another surface owns the desk (Quick Settings, the overview, plain
    /// search, a sheet): widgets stay away under it.
    pub covered: bool,
    /// The widget under the hand: the pointer is on it, or a press on it is
    /// held. It keeps its form.
    pub engaged: Option<WidgetId>,
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
    opened: Option<WidgetId>,
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
    /// card the pointer is not on folds too); reaching for one is
    /// forgotten.
    pub fn note_typed(&mut self, at: Instant, slots: &[Slot]) {
        if self.last_typed.is_some_and(|last| last >= at) {
            return;
        }
        self.last_typed = Some(at);
        self.reached = None;
        let on_open = self.opened.is_some_and(|id| {
            self.pointer.is_some_and(|p| {
                slots
                    .iter()
                    .any(|slot| slot.id == id && slot.open.contains(p))
            })
        });
        if !on_open {
            self.opened = None;
        }
    }

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

    pub fn pointer_left(&mut self) {
        self.pointer = None;
        self.in_reach = false;
        self.aim.clear();
    }

    /// Open `id` on purpose (a tap on its glance form, Enter, an action).
    pub fn open(&mut self, id: WidgetId) {
        self.opened = Some(id);
    }

    /// Fold whatever is open back to its glance form.
    pub fn close(&mut self) {
        self.opened = None;
    }

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
        let mut changed = false;
        for slot in ctx.slots {
            let before = self.form(slot.id);
            let mut form = self.decide(slot, ctx);
            // Under the hand a widget keeps its form, whatever else
            // changed: it never flips between a press and its release, or
            // while the pointer is on it. Only a widget that was showing
            // holds; one that is away holds nothing to keep. Asking for it
            // (opening, closing, Super, removing it) still goes through.
            let asked = ctx.super_surface || ctx.super_glance || self.opened == Some(slot.id);
            let removed = !ctx.prefs.get(slot.id).enabled || !slot.has_content;
            if ctx.engaged == Some(slot.id)
                && !removed
                && before != Form::Away
                && form != before
                && !(asked && form == Form::Open)
                && !(before == Form::Open && self.opened != Some(slot.id) && form == Form::Glance)
            {
                form = before;
            }
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
        // Settling only matters while something waits on it: a held widget
        // or a pointer that may be resting in a reach zone.
        let settle = (!self.held.is_empty() || self.pointer.is_some())
            .then(|| self.aim.deadline(now))
            .flatten();
        [pause, reach, settle]
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
        engaged: Option<WidgetId>,
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
                engaged: None,
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
                super_surface: self.super_surface,
                super_glance: self.super_glance,
                covered: false,
                engaged: self.engaged,
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

    /// Resting on a glance, however long, is not a request; a tap is, and
    /// only a tap (or Escape, or typing away from it) folds it again.
    #[test]
    fn only_a_tap_opens_a_glance_and_nothing_folds_it_on_a_timer() {
        let t0 = Instant::now();
        let mut rig = Rig::new();
        assert_eq!(rig.at(t0).0, Form::Glance);
        rig.pointer((1300.0, 860.0), t0);
        rig.pointer((1400.0, 868.0), ms(t0, 8));
        rig.pointer((1401.0, 868.0), ms(t0, 40));
        assert_eq!(
            rig.at(ms(t0, 5_000)).0,
            Form::Glance,
            "a long rest opens nothing"
        );
        rig.board.open(WidgetId::Music);
        assert_eq!(rig.at(ms(t0, 5_010)).0, Form::Open);
        rig.pointer((700.0, 300.0), ms(t0, 5_100));
        assert_eq!(
            rig.at(ms(t0, 60_000)).0,
            Form::Open,
            "leaving the card folds nothing: only a tap away does"
        );
        rig.board.close();
        assert_eq!(rig.at(ms(t0, 60_010)).0, Form::Glance);
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

    /// Under the hand a widget never flips: typing while the pointer is on
    /// a glance keeps it, and it goes once the hand leaves.
    #[test]
    fn a_widget_under_the_hand_keeps_its_form() {
        let t0 = Instant::now();
        let mut rig = Rig::new();
        assert_eq!(rig.at(t0), (Form::Glance, Form::Glance));
        rig.engaged = Some(WidgetId::Music);
        rig.typed(ms(t0, 10));
        assert_eq!(
            rig.at(ms(t0, 10)),
            (Form::Glance, Form::Away),
            "the one under the hand stays; the rest fade"
        );
        rig.board.open(WidgetId::Music);
        assert_eq!(rig.at(ms(t0, 20)).0, Form::Open, "a tap still opens it");
        rig.engaged = None;
        rig.typed(ms(t0, 30));
        assert_eq!(
            rig.at(ms(t0, 30)).0,
            Form::Away,
            "hand gone: typing hides it"
        );
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
