//! The reveal kernel every collapsible panel shares: `NavShell`'s sidebar
//! and inspector, and the headless [`crate::widget::SidebarDrawer`].
//!
//! A flap tracks whether it should be open and eases a `reveal` value in
//! `0..=1` toward that over the theme's `motion.durations.base`. Whether a
//! flap is open follows its layout's default until the person says
//! otherwise; that choice is remembered per layout (per window size class),
//! so a sidebar hidden in a wide window is still hidden after the window was
//! narrowed and widened again. A drawer opened over a narrow window is
//! transient: crossing into another layout closes it.

use crate::time::{Duration, Instant};

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

/// Settles within this distance of its target.
const SETTLED: f64 = 0.001;

/// The eased reveal shared by every flap. See the module docs.
#[derive(Clone, Debug)]
pub struct FlapState {
    /// The person's choice while docked; kept across layout changes.
    docked_choice: Option<bool>,
    /// The person's choice while an overlay; dropped on a layout change.
    overlay_choice: Option<bool>,
    /// Linear progress toward the target, `0..=1`; [`Self::reveal`] eases it.
    progress: f64,
    last_layout: Option<SplitLayout>,
    /// Whether the flap is open by default when docked. Overlay always
    /// defaults closed.
    docked_default_open: bool,
    /// When the reveal was last advanced, or when its slide began; `None`
    /// at rest. The first frame of a slide covers the time since the
    /// person asked for it, never the idle gap before ([`Self::advance`]).
    clock: Option<Instant>,
}

impl FlapState {
    /// A flap that is open when docked (the common navigation sidebar).
    pub fn open_when_docked() -> Self {
        Self {
            docked_choice: None,
            overlay_choice: None,
            progress: 1.0,
            last_layout: None,
            docked_default_open: true,
            clock: None,
        }
    }

    /// A flap that is closed even when docked — summoned on demand.
    pub fn closed_by_default() -> Self {
        Self {
            docked_choice: None,
            overlay_choice: None,
            progress: 0.0,
            last_layout: None,
            docked_default_open: false,
            clock: None,
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
        // A slide starts now; one already moving keeps its clock.
        if self.clock.is_none() {
            self.clock = Some(crate::time::now());
        }
        match layout {
            SplitLayout::Docked => self.docked_choice = Some(open),
            SplitLayout::Overlay => self.overlay_choice = Some(open),
        }
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
        self.progress = if open { 1.0 } else { 0.0 };
        self.clock = None;
    }

    /// Jump the reveal to the resolved openness without recording a
    /// choice.
    pub fn settle(&mut self, layout: SplitLayout) {
        self.progress = if self.is_open(layout) { 1.0 } else { 0.0 };
        self.clock = None;
    }

    /// Start the reveal over from closed, so an open flap that just came
    /// into a layout (a sidebar that docks as the window widens) slides in
    /// rather than appearing. A closed flap stays as it is.
    pub(crate) fn slide_in(&mut self, layout: SplitLayout) {
        if !self.is_open(layout) {
            return;
        }
        self.progress = 0.0;
        self.clock = Some(crate::time::now());
    }

    /// Advance toward the target to `now` (the frame's time), taking
    /// `duration` for a whole open or close. The step is the time since the
    /// last frame of this slide, or since it was asked for — never the idle
    /// gap before, and never clamped: a stalled frame lands where the
    /// slide's own clock says, as a wall-clock tween does. Returns whether
    /// it is still moving.
    pub fn advance(&mut self, layout: SplitLayout, now: Instant, duration: Duration) -> bool {
        let dt = self.clock.replace(now).map_or(0.0, |last| {
            now.saturating_duration_since(last).as_secs_f64()
        });
        let moving = self.tick(layout, dt, duration);
        if !moving {
            self.clock = None;
        }
        moving
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

    /// Advance toward the target by `dt`, taking `duration` for a whole
    /// open or close. Returns whether it is still moving.
    pub fn tick(&mut self, layout: SplitLayout, dt: f64, duration: Duration) -> bool {
        let target = if self.is_open(layout) { 1.0 } else { 0.0 };
        let span = duration.as_secs_f64();
        if span <= 0.0 {
            self.progress = target;
            return false;
        }
        let step = dt.max(0.0) / span;
        self.progress = if target > self.progress {
            (self.progress + step).min(target)
        } else {
            (self.progress - step).max(target)
        };
        if (self.progress - target).abs() < SETTLED {
            // A settled panel must land at its exact endpoint. Leaving the
            // sub-threshold remainder would stop scheduling frames while
            // its pointer targets still sit outside the resolved geometry.
            self.progress = target;
            false
        } else {
            true
        }
    }

    /// The eased reveal in `0..=1`: the docked width share, the overlay
    /// slide and the scrim strength.
    pub fn reveal(&self) -> f64 {
        crate::ease_out(self.progress.clamp(0.0, 1.0))
    }

    /// The reveal as it stands in `layout`: the eased value while the flap
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
        // Keep scheduling until tick has snapped the final fraction. A
        // threshold here could stop the host before that last tick runs.
        !self.progress.total_cmp(&target).is_eq()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: Duration = Duration::from_millis(210);

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
        assert!(!flap.advance(layout, idle_frame, BASE), "at rest");
        flap.open();
        let toggled = flap.clock.expect("a choice starts the slide's clock");
        let first_frame = toggled + Duration::from_millis(11);
        assert!(flap.advance(layout, first_frame, BASE));
        let expected = 11.0 / 210.0;
        assert!(
            (flap.progress - expected).abs() < 1e-9,
            "one frame of progress, got {}",
            flap.progress
        );
        // A stall is covered, not slowed: the slide ends on its own clock.
        assert!(!flap.advance(layout, first_frame + BASE, BASE));
        assert_eq!(flap.reveal(), 1.0);
        assert_eq!(flap.clock, None, "a settled flap holds no clock");
    }

    #[test]
    fn settled_reveal_reaches_both_endpoints_before_frames_stop() {
        for layout in [SplitLayout::Docked, SplitLayout::Overlay] {
            let mut flap = FlapState::closed_by_default();
            flap.reconcile(layout);
            flap.open();
            let almost_complete = BASE.as_secs_f64() * (1.0 - SETTLED / 2.0);
            assert!(!flap.tick(layout, almost_complete, BASE));
            assert!(!flap.is_animating(layout));
            assert_eq!(flap.reveal_in(layout), 1.0);
            flap.close();
            assert!(!flap.tick(layout, almost_complete, BASE));
            assert!(!flap.is_animating(layout));
            assert_eq!(flap.reveal_in(layout), 0.0);
        }
    }

    #[test]
    fn a_subthreshold_remainder_still_requests_its_final_settlement_tick() {
        for layout in [SplitLayout::Docked, SplitLayout::Overlay] {
            let mut flap = FlapState::closed_by_default();
            flap.reconcile(layout);
            flap.open();
            // The host can observe animation state between ticks, including
            // a remainder produced before the endpoint snap was introduced.
            flap.progress = 1.0 - SETTLED / 2.0;
            assert!(flap.is_animating(layout));
            assert!(!flap.tick(layout, 0.0, BASE));
            assert_eq!(flap.reveal_in(layout), 1.0);
            assert!(!flap.is_animating(layout));
            flap.close();
            flap.progress = SETTLED / 2.0;
            assert!(flap.is_animating(layout));
            assert!(!flap.tick(layout, 0.0, BASE));
            assert_eq!(flap.reveal_in(layout), 0.0);
            assert!(!flap.is_animating(layout));
        }
    }

    #[test]
    fn the_reveal_takes_the_motion_duration() {
        let mut flap = FlapState::closed_by_default();
        flap.reconcile(SplitLayout::Docked);
        flap.open();
        assert!(flap.tick(SplitLayout::Docked, 0.1, BASE));
        let mid = flap.reveal();
        assert!(mid > 0.0 && mid < 1.0);
        assert!(!flap.tick(SplitLayout::Docked, 0.2, BASE));
        assert_eq!(flap.reveal(), 1.0);
        flap.close();
        assert!(!flap.tick(SplitLayout::Docked, 0.0, Duration::ZERO));
        assert_eq!(flap.reveal(), 0.0, "no duration is an instant change");
    }
}
