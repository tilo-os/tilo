//! The reveal kernel every collapsible panel shares: `NavShell`'s sidebar and inspector,
//! and the headless [`crate::widget::SidebarDrawer`].

use crate::motion::{Direction, MotionValue, PanelSlide};
use crate::time::Instant;
use crate::tokens::{EffectiveMotion, MotionSource};

/// How a sidebar resolves at the current width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitLayout {
    Docked,
    Overlay,
}

impl SplitLayout {
    /// Boundary is inclusive at the breakpoint → `Docked`.
    pub fn from_width(total_w: f64, breakpoint: f64) -> Self {
        if total_w >= breakpoint {
            SplitLayout::Docked
        } else {
            SplitLayout::Overlay
        }
    }
}

/// The reveal shared by every flap.
#[derive(Clone, Debug)]
pub struct FlapState {
    docked_choice: Option<bool>,
    overlay_choice: Option<bool>,
    openness: MotionValue<f64>,
    drawn: f64,
    drawn_at: Option<Instant>,
    last_layout: Option<SplitLayout>,
    docked_default_open: bool,
    /// When the last choice not yet followed was made, and by what input:
    /// the slide starts then, so the first frame after an idle app covers
    /// the time since the person asked, never the idle gap before.
    asked: Option<(Instant, MotionSource)>,
}

impl FlapState {
    /// A flap that is open when docked (the common navigation sidebar).
    pub fn open_when_docked() -> Self {
        Self::starting(1.0, true)
    }

    /// A flap that is closed even when docked — summoned on demand.
    pub fn closed_by_default() -> Self {
        Self::starting(0.0, false)
    }

    fn starting(reveal: f64, docked_default_open: bool) -> Self {
        Self {
            docked_choice: None,
            overlay_choice: None,
            openness: MotionValue::new(reveal),
            drawn: reveal,
            drawn_at: None,
            last_layout: None,
            docked_default_open,
            asked: None,
        }
    }

    fn choice(&self, layout: SplitLayout) -> Option<bool> {
        match layout {
            SplitLayout::Docked => self.docked_choice,
            SplitLayout::Overlay => self.overlay_choice,
        }
    }

    fn choose(&mut self, open: bool) {
        self.choose_in(self.last_layout.unwrap_or(SplitLayout::Docked), open);
    }

    fn choose_in(&mut self, layout: SplitLayout, open: bool) {
        let source = self
            .asked
            .map_or(MotionSource::Pointer, |(_, source)| source);
        self.asked = Some((crate::time::now(), source));
        match layout {
            SplitLayout::Docked => self.docked_choice = Some(open),
            SplitLayout::Overlay => self.overlay_choice = Some(open),
        }
    }

    /// Say what input the next choice comes from: a shortcut lands at once.
    pub fn from_input(&mut self, source: MotionSource) {
        let at = self.asked.map_or_else(crate::time::now, |(at, _)| at);
        self.asked = Some((at, source));
    }

    /// Whether the flap is currently open in `layout`, resolving the person's choice
    /// against the layout default.
    pub fn is_open(&self, layout: SplitLayout) -> bool {
        self.choice(layout).unwrap_or(match layout {
            SplitLayout::Docked => self.docked_default_open,
            SplitLayout::Overlay => false,
        })
    }

    /// Flip the flap relative to its resolved openness in `layout`.
    pub fn toggle(&mut self, layout: SplitLayout) {
        let open = !self.is_open(layout);
        self.choose_in(layout, open);
    }

    /// Open the flap in the current layout.
    pub fn open(&mut self) {
        self.choose(true);
    }

    /// Close the flap in the current layout (scrim, Esc, a choice made in a drawer).
    pub fn close(&mut self) {
        self.choose(false);
    }

    /// Set the open state with no animation, for deterministic shots.
    pub fn snap(&mut self, open: bool) {
        self.choose(open);
        self.land(if open { 1.0 } else { 0.0 });
    }

    /// Jump the reveal to the resolved openness without recording a choice.
    pub fn settle(&mut self, layout: SplitLayout) {
        self.land(if self.is_open(layout) { 1.0 } else { 0.0 });
    }

    fn land(&mut self, reveal: f64) {
        self.openness.snap(reveal);
        self.drawn = reveal;
        self.asked = None;
    }

    /// Start the reveal over from closed, so an open flap that just came into a layout (a
    /// sidebar that docks as the window widens) slides in rather than appearing.
    pub(crate) fn slide_in(&mut self, layout: SplitLayout) {
        if !self.is_open(layout) {
            return;
        }
        self.land(0.0);
        self.asked = Some((crate::time::now(), MotionSource::Pointer));
    }

    /// Follow the resolved state to `now` (the frame's time) on the panel slide `motion`
    /// allows.
    pub fn advance(&mut self, layout: SplitLayout, now: Instant, motion: EffectiveMotion) -> bool {
        let target = if self.is_open(layout) { 1.0 } else { 0.0 };
        if let Some((asked, source)) = self.asked.take() {
            let direction = Direction::of(target > self.openness.target());
            let timing = PanelSlide::SPEC.timing(direction, motion, source);
            // The slide starts when it was asked for, from where it was
            // drawn then: never before the last frame drawn, never after
            // this one.
            let start = self
                .drawn_at
                .map_or(asked, |drawn| asked.max(drawn))
                .min(now);
            self.openness.set(target, timing, start);
        } else if (self.openness.target() - target).abs() > f64::EPSILON
            || (motion.reduced() && self.openness.is_moving(now))
        {
            self.openness.snap(target);
        }
        self.drawn = self.openness.get(now).clamp(0.0, 1.0);
        self.drawn_at = Some(now);
        self.openness.is_moving(now)
    }

    /// Note the layout the flap is laid out in. The first call settles the
    /// reveal (the constructor guessed before any width was known); a later
    /// change drops a transient overlay choice and settles to the new
    /// layout's resolved state, so a resize is a structural jump, never a
    /// slide.
    pub fn reconcile(&mut self, layout: SplitLayout) {
        if self.last_layout == Some(layout) {
            return;
        }
        if self.last_layout.is_some() {
            self.overlay_choice = None;
        }
        self.last_layout = Some(layout);
        self.settle(layout);
    }

    /// The reveal in `0..=1` as of the last frame: the docked width share, the overlay
    /// slide and the scrim strength.
    pub fn reveal(&self) -> f64 {
        self.drawn
    }

    /// The reveal as it stands in `layout`: the drawn value while the flap
    /// is laid out there, else where `layout`'s resolved state would put it
    /// (a layout the flap has not been reconciled to yet does not slide).
    pub(crate) fn reveal_in(&self, layout: SplitLayout) -> f64 {
        if self.last_layout.is_none_or(|last| last == layout) {
            self.reveal()
        } else if self.is_open(layout) {
            1.0
        } else {
            0.0
        }
    }

    /// Whether the reveal is still moving toward the resolved state.
    pub fn is_animating(&self, layout: SplitLayout) -> bool {
        if self.last_layout.is_some_and(|last| last != layout) {
            return false;
        }
        let target = if self.is_open(layout) { 1.0 } else { 0.0 };
        !self.drawn.total_cmp(&target).is_eq()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Duration;
    use crate::tokens::Motion;

    fn motion() -> EffectiveMotion {
        EffectiveMotion::new(Motion::default(), false)
    }

    #[test]
    fn a_choice_is_remembered_per_size_class() {
        let mut flap = FlapState::open_when_docked();
        flap.reconcile(SplitLayout::Docked);
        assert!(flap.is_open(SplitLayout::Docked));
        flap.toggle(SplitLayout::Docked);
        assert!(!flap.is_open(SplitLayout::Docked));

        flap.reconcile(SplitLayout::Overlay);
        assert!(!flap.is_open(SplitLayout::Overlay));
        flap.open();
        assert!(flap.is_open(SplitLayout::Overlay));

        flap.reconcile(SplitLayout::Docked);
        assert!(!flap.is_open(SplitLayout::Docked));
        assert_eq!(flap.reveal(), 0.0, "a resize jumps, it does not slide");

        flap.reconcile(SplitLayout::Overlay);
        assert!(!flap.is_open(SplitLayout::Overlay));
    }

    #[test]
    fn the_first_frame_after_idle_advances_from_the_toggle() {
        let layout = SplitLayout::Docked;
        let mut flap = FlapState::closed_by_default();
        flap.reconcile(layout);
        let idle_frame = crate::time::now();
        assert!(!flap.advance(layout, idle_frame, motion()), "at rest");
        flap.open();
        let (toggled, _) = flap.asked.expect("a choice starts the slide");
        let first_frame = toggled + Duration::from_millis(11);
        assert!(flap.advance(layout, first_frame, motion()));
        let first = flap.reveal();
        assert!(
            first > 0.0 && first < 0.2,
            "one frame of progress, got {first}"
        );
        assert!(!flap.advance(layout, first_frame + Duration::from_secs(1), motion()));
        assert_eq!(flap.reveal(), 1.0);
        assert!(!flap.is_animating(layout));
    }

    /// Reduced motion turned on mid-slide lands the slide on the next frame:
    /// a panel never keeps moving after the person asked for nothing to
    /// move, and what is laid out over it (a drawer's rows) is where it is
    /// drawn.
    #[test]
    fn a_slide_under_way_lands_when_motion_is_reduced() {
        let layout = SplitLayout::Docked;
        let mut flap = FlapState::closed_by_default();
        flap.reconcile(layout);
        flap.open();
        let (toggled, _) = flap.asked.expect("a choice starts the slide");
        let first_frame = toggled + Duration::from_millis(11);
        assert!(flap.advance(layout, first_frame, motion()), "sliding");
        assert!(flap.reveal() < 1.0);
        let reduced = EffectiveMotion::new(Motion::default(), true);
        assert!(
            !flap.advance(layout, first_frame + Duration::from_millis(11), reduced),
            "the next frame under reduced motion is at rest"
        );
        assert_eq!(flap.reveal(), 1.0);
        assert!(!flap.is_animating(layout));
    }

    #[test]
    fn a_layout_the_flap_has_not_reconciled_to_does_not_move() {
        let docked = SplitLayout::Docked;
        let mut flap = FlapState::open_when_docked();
        flap.reconcile(docked);
        flap.close();
        let (asked, _) = flap.asked.expect("asked");
        flap.advance(docked, asked + Duration::from_millis(11), motion());
        let drawn = flap.reveal();
        assert!(drawn > 0.0 && drawn < 1.0, "sliding, got {drawn}");

        assert_eq!(flap.reveal_in(SplitLayout::Overlay), 0.0);
        assert!(
            !flap.is_animating(SplitLayout::Overlay),
            "a layout it has not reconciled to stands at its resolved reveal"
        );

        let overlay = SplitLayout::Overlay;
        let mut flap = FlapState::open_when_docked();
        flap.reconcile(overlay);
        flap.open();
        let (asked, _) = flap.asked.expect("asked");
        flap.advance(overlay, asked + Duration::from_millis(11), motion());
        assert!(flap.reveal_in(overlay) > 0.0 && flap.reveal_in(overlay) < 1.0);
        assert_eq!(flap.reveal_in(docked), 1.0);
        assert!(!flap.is_animating(docked));
    }

    #[test]
    fn closing_mirrors_opening() {
        let layout = SplitLayout::Docked;
        let mut flap = FlapState::closed_by_default();
        flap.reconcile(layout);
        flap.open();
        let (t0, _) = flap.asked.expect("asked");
        flap.advance(layout, t0 + Duration::from_millis(40), motion());
        let opened = flap.reveal();
        let t1 = t0 + Duration::from_secs(1);
        flap.advance(layout, t1, motion());
        flap.close();
        flap.asked = Some((t1, MotionSource::Pointer));
        flap.advance(layout, t1 + Duration::from_millis(40), motion());
        let closed = 1.0 - flap.reveal();
        assert!((opened - closed).abs() < 1e-6, "{opened} vs {closed}");
        assert!(opened > 0.3, "it gets going at once: {opened}");
    }

    #[test]
    fn a_reversal_mid_slide_continues_from_where_it_is() {
        let layout = SplitLayout::Docked;
        let mut flap = FlapState::closed_by_default();
        flap.reconcile(layout);
        flap.open();
        let (t0, _) = flap.asked.expect("asked");
        let mid = t0 + Duration::from_millis(50);
        flap.advance(layout, mid, motion());
        let drawn = flap.reveal();
        flap.close();
        flap.asked = Some((mid, MotionSource::Pointer));
        flap.advance(layout, mid, motion());
        assert!((flap.reveal() - drawn).abs() < 1e-9, "no jump");
        flap.advance(layout, mid + Duration::from_millis(8), motion());
        assert!(
            flap.reveal() >= drawn - 1e-3,
            "it brakes before turning back"
        );
    }

    #[test]
    fn settled_reveal_reaches_both_endpoints_before_frames_stop() {
        for layout in [SplitLayout::Docked, SplitLayout::Overlay] {
            let mut flap = FlapState::closed_by_default();
            flap.reconcile(layout);
            flap.open();
            let later = crate::time::now() + Duration::from_secs(2);
            assert!(
                flap.is_animating(layout),
                "asks for the frame that moves it"
            );
            assert!(!flap.advance(layout, later, motion()));
            assert!(!flap.is_animating(layout));
            assert_eq!(flap.reveal_in(layout), 1.0);
            flap.close();
            flap.asked = Some((later, MotionSource::Pointer));
            assert!(!flap.advance(layout, later + Duration::from_secs(2), motion()));
            assert!(!flap.is_animating(layout));
            assert_eq!(flap.reveal_in(layout), 0.0);
        }
    }

    #[test]
    fn a_shortcut_and_reduced_motion_land_at_once() {
        let layout = SplitLayout::Docked;
        let now = crate::time::now();
        let mut flap = FlapState::closed_by_default();
        flap.reconcile(layout);
        flap.from_input(MotionSource::Keyboard);
        flap.open();
        assert!(!flap.advance(layout, now, motion()));
        assert_eq!(flap.reveal(), 1.0);
        flap.close();
        let reduced = EffectiveMotion::new(Motion::default(), true);
        assert!(!flap.advance(layout, now, reduced));
        assert_eq!(flap.reveal(), 0.0);
    }
}
