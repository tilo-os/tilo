//! The reveal kernel every collapsible panel shares: `NavShell`'s sidebar
//! and inspector, and the headless [`crate::widget::SidebarDrawer`].
//!
//! A flap tracks whether it should be open, and its reveal (`0..=1`) follows
//! that on the standard panel slide ([`crate::motion::PanelSlide`]):
//! interruptible, the same both ways, landing at once for a shortcut and
//! under reduced motion. Whether a flap is open follows its layout's default
//! until the person says otherwise; that choice is remembered per layout
//! (per window size class), so a sidebar hidden in a wide window is still
//! hidden after the window was narrowed and widened again. A drawer opened
//! over a narrow window is transient: crossing into another layout closes it.

use crate::motion::{Direction, MotionValue, PanelSlide};
use crate::time::Instant;
use crate::tokens::{EffectiveMotion, MotionSource};

/// How a sidebar resolves at the current width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitLayout {
    /// Beside the content.
    Docked,
    /// Over the content, as a drawer.
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

/// The reveal shared by every flap. See the module docs.
#[derive(Clone, Debug)]
pub struct FlapState {
    /// The person's choice while docked; kept across layout changes.
    docked_choice: Option<bool>,
    /// The person's choice while an overlay; dropped on a layout change.
    overlay_choice: Option<bool>,
    /// The reveal on the panel slide.
    openness: MotionValue<f64>,
    /// The reveal as of the last [`Self::advance`]: what layout and paint use.
    drawn: f64,
    /// The frame time of the last [`Self::advance`].
    drawn_at: Option<Instant>,
    last_layout: Option<SplitLayout>,
    /// Whether the flap is open by default when docked. Overlay always
    /// defaults closed.
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

    /// Whether the flap is currently open in `layout`, resolving the
    /// person's choice against the layout default.
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

    /// Close the flap in the current layout (scrim, Esc, a choice made in a
    /// drawer).
    pub fn close(&mut self) {
        self.choose(false);
    }

    /// Set the open state with no animation, for deterministic shots.
    pub fn snap(&mut self, open: bool) {
        self.choose(open);
        self.land(if open { 1.0 } else { 0.0 });
    }

    /// Jump the reveal to the resolved openness without recording a
    /// choice.
    pub fn settle(&mut self, layout: SplitLayout) {
        self.land(if self.is_open(layout) { 1.0 } else { 0.0 });
    }

    fn land(&mut self, reveal: f64) {
        self.openness.snap(reveal);
        self.drawn = reveal;
        self.asked = None;
    }

    /// Start the reveal over from closed, so an open flap that just came
    /// into a layout (a sidebar that docks as the window widens) slides in
    /// rather than appearing. A closed flap stays as it is.
    pub(crate) fn slide_in(&mut self, layout: SplitLayout) {
        if !self.is_open(layout) {
            return;
        }
        self.land(0.0);
        self.asked = Some((crate::time::now(), MotionSource::Pointer));
    }

    /// Follow the resolved state to `now` (the frame's time) on the panel
    /// slide `motion` allows. Returns whether it is still moving.
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
        } else if (self.openness.target() - target).abs() > f64::EPSILON {
            // The resolved state moved without a choice (a layout default).
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

    /// The reveal in `0..=1` as of the last frame: the docked width share,
    /// the overlay slide and the scrim strength.
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
        let target = if self.is_open(layout) { 1.0 } else { 0.0 };
        // Keep scheduling until advance has landed exactly: a threshold
        // here could stop the host before that last frame runs.
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

        // Narrow: a drawer, closed by default; opening it is transient.
        flap.reconcile(SplitLayout::Overlay);
        assert!(!flap.is_open(SplitLayout::Overlay));
        flap.open();
        assert!(flap.is_open(SplitLayout::Overlay));

        // Wide again: still hidden, as the person left it.
        flap.reconcile(SplitLayout::Docked);
        assert!(!flap.is_open(SplitLayout::Docked));
        assert_eq!(flap.reveal(), 0.0, "a resize jumps, it does not slide");

        // And the drawer did not stay open behind the person's back.
        flap.reconcile(SplitLayout::Overlay);
        assert!(!flap.is_open(SplitLayout::Overlay));
    }

    /// After the app sat idle, the first frame of a slide moves by the time
    /// since the toggle — not by the idle gap (which used to open a
    /// sidebar 86 % in one frame).
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
        // A stall is covered, not slowed: the slide ends on its own clock.
        assert!(!flap.advance(layout, first_frame + Duration::from_secs(1), motion()));
        assert_eq!(flap.reveal(), 1.0);
        assert!(!flap.is_animating(layout));
    }

    /// The panel slide is the same gesture both ways: closing no longer
    /// starts slowly and slams shut (the old curve was an ease-out played
    /// backwards, an ease-in).
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
        // Asked for on the frame it closed at (the test's clock runs ahead
        // of the real one).
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
