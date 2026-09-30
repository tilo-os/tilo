//! Adaptive chrome — a GNOME Adwaita `OverlaySplitView` ("flap") for the
//! navigation panes.
//!
//! Chat has two navigation panes that can either sit beside the
//! conversation (wide window) or slide in over it behind a dimming scrim
//! (narrow window): the **left flap** (workspace rail + channel sidebar)
//! and the **right flap** (members / thread panel). The choice is made from
//! the widget's *allocated* width, not a fixed config:
//!
//! * **Docked** (wide window): the pane is permanently beside the content,
//!   which is inset to make room. The desktop case.
//! * **Overlay** (narrow window): the content uses the full width and the
//!   pane, when opened, slides in *over* it behind a scrim — exactly like
//!   Adwaita's flap on a phone-width window. It is **closed by default** at
//!   this size so a narrow window opens straight to the conversation.
//!
//! A user toggle (the hamburger / members button) flips the pane; the
//! [`FlapState`] eases a 0→1 `reveal` every frame so the slide and scrim
//! fade are smooth. When the window crosses the breakpoint the pane adopts
//! the new mode's default unless the user expressed an explicit preference.
//!
//! This mirrors the weather app's adaptive sidebar, generalised to a pair
//! of opposing flaps.

use ui::time::Instant;

/// Below this allocated width (logical px) the left flap (rail + channel
/// sidebar) switches from docked to overlay. Chosen so the rail + sidebar +
/// a readable conversation column stop fitting comfortably side by side.
pub const LEFT_BREAKPOINT: f64 = 760.0;

/// Below this allocated width the right flap (members / thread) goes
/// overlay. Set higher than the left breakpoint so a medium window keeps
/// the channel list docked but floats the members panel.
pub const RIGHT_BREAKPOINT: f64 = 1040.0;

/// How a flap relates to the content at the current window size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlapMode {
    /// Wide window: pane is permanently beside the (inset) content.
    Docked,
    /// Narrow window: pane overlays the full-width content behind a scrim.
    Overlay,
}

impl FlapMode {
    /// Decide the mode purely from the allocated width against a breakpoint.
    pub fn from_width(total_w: f64, breakpoint: f64) -> Self {
        if total_w >= breakpoint {
            FlapMode::Docked
        } else {
            FlapMode::Overlay
        }
    }
}

/// The animated open/closed state of one flap, plus the user's explicit
/// preference (if any) for this session.
#[derive(Clone, Debug)]
pub struct FlapState {
    /// `Some(true/false)` once the user has toggled; `None` means "follow
    /// the mode's default" (docked ⇒ open, overlay ⇒ closed).
    user_open: Option<bool>,
    /// Eased reveal: 0 = closed, 1 = open. Drives the slide + scrim alpha.
    reveal: f64,
    /// Last mode we resolved, to detect breakpoint crossings.
    last_mode: Option<FlapMode>,
    last_tick: Instant,
    /// Default openness when no preference and docked (left flap ⇒ true,
    /// right flap ⇒ false so the members panel doesn't crowd a fresh frame).
    docked_default_open: bool,
}

impl FlapState {
    /// A flap that defaults open when docked (the left navigation flap).
    pub fn open_when_docked() -> Self {
        Self::with_default(true)
    }

    /// A flap that defaults closed even when docked (the right members
    /// flap, summoned on demand).
    pub fn closed_by_default() -> Self {
        Self::with_default(false)
    }

    fn with_default(docked_default_open: bool) -> Self {
        Self {
            user_open: None,
            // Resolve the initial reveal to the docked default so a
            // desktop-width first frame (and `--shots`, 1280px) shows the
            // pane without animating in.
            reveal: if docked_default_open { 1.0 } else { 0.0 },
            last_mode: None,
            last_tick: ui::time::now(),
            docked_default_open,
        }
    }

    /// Whether the flap should currently be open in `mode`, honouring the
    /// user's preference and falling back to the mode default.
    pub fn is_open(&self, mode: FlapMode) -> bool {
        self.user_open.unwrap_or(match mode {
            FlapMode::Docked => self.docked_default_open,
            FlapMode::Overlay => false,
        })
    }

    /// Flip the user preference relative to the current resolved openness.
    pub fn toggle(&mut self, mode: FlapMode) {
        let now_open = self.is_open(mode);
        self.user_open = Some(!now_open);
    }

    /// Force the flap open (used when a structural action needs the pane
    /// visible, e.g. opening a thread).
    pub fn open(&mut self) {
        self.user_open = Some(true);
    }

    /// Force the flap closed (e.g. tapping the scrim in overlay mode).
    pub fn close(&mut self) {
        self.user_open = Some(false);
    }

    /// Snap to a final open/closed state with no animation. Used by the
    /// single-frame `--shots` renderer to capture a settled flap rather than
    /// one mid-slide (the live app eases via [`Self::tick`]).
    pub fn snap(&mut self, open: bool) {
        self.user_open = Some(open);
        self.reveal = if open { 1.0 } else { 0.0 };
    }

    /// Reconcile with the latest mode: on a breakpoint crossing, drop any
    /// stale user preference so the flap adopts the new mode's default.
    ///
    /// The **first** resolution (once the real allocated width is known) also
    /// snaps the eased `reveal` straight to the resolved open/closed state
    /// rather than animating into it. Without this a narrow first frame would
    /// paint the docked-open default (`reveal` seeded to 1.0 for the desktop
    /// case) and then slide it closed — a visible flash of the rail + sidebar
    /// over the conversation on a phone-width launch. Snapping settles the
    /// correct state on frame one; genuine later crossings still animate.
    pub fn reconcile(&mut self, mode: FlapMode) {
        let first = self.last_mode.is_none();
        if self.last_mode != Some(mode) {
            if self.last_mode.is_some() {
                self.user_open = None;
            }
            self.last_mode = Some(mode);
        }
        if first {
            self.reveal = if self.is_open(mode) { 1.0 } else { 0.0 };
        }
    }

    /// Advance the eased reveal toward the target using real elapsed time
    /// (frame-rate independent). Returns true while still animating.
    pub fn tick(&mut self, mode: FlapMode) -> bool {
        let now = ui::time::now();
        let dt = (now - self.last_tick).as_secs_f64().min(0.1);
        self.last_tick = now;
        self.tick_with_dt(mode, dt)
    }

    fn tick_with_dt(&mut self, mode: FlapMode, dt: f64) -> bool {
        let target = if self.is_open(mode) { 1.0 } else { 0.0 };
        let rate = 14.0; // ~0.2s Adwaita-like slide
        let alpha = 1.0 - (-rate * dt).exp();
        self.reveal += (target - self.reveal) * alpha;
        if (self.reveal - target).abs() < 0.001 {
            self.reveal = target;
            false
        } else {
            true
        }
    }

    pub fn reveal(&self) -> f64 {
        self.reveal.clamp(0.0, 1.0)
    }

    /// Reveal smoothed once more (ease-out cubic) for the slide offset.
    pub fn slide(&self) -> f64 {
        let r = self.reveal();
        1.0 - (1.0 - r).powi(3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flap_mode_switches_at_the_breakpoint_and_each_side_has_its_default() {
        // Phase: mode_switches_at_breakpoint.
        {
            assert_eq!(
                FlapMode::from_width(1280.0, LEFT_BREAKPOINT),
                FlapMode::Docked
            );
            assert_eq!(
                FlapMode::from_width(700.0, LEFT_BREAKPOINT),
                FlapMode::Overlay
            );
            assert_eq!(
                FlapMode::from_width(LEFT_BREAKPOINT, LEFT_BREAKPOINT),
                FlapMode::Docked
            );
            assert_eq!(
                FlapMode::from_width(LEFT_BREAKPOINT - 1.0, LEFT_BREAKPOINT),
                FlapMode::Overlay
            );
        }
        // Phase: left_flap_defaults_open_docked_closed_overlay.
        {
            let s = FlapState::open_when_docked();
            assert!(s.is_open(FlapMode::Docked), "wide window shows the rail");
            assert!(
                !s.is_open(FlapMode::Overlay),
                "narrow window hides it by default"
            );
        }
        // Phase: right_flap_defaults_closed_even_docked.
        {
            let s = FlapState::closed_by_default();
            assert!(
                !s.is_open(FlapMode::Docked),
                "members panel summoned on demand"
            );
            assert!(!s.is_open(FlapMode::Overlay));
        }
    }

    #[test]
    fn flap_state_toggles_resets_and_settles_across_breakpoints() {
        // Phase: toggle_flips_relative_to_current.
        {
            let mut s = FlapState::open_when_docked();
            s.toggle(FlapMode::Overlay);
            assert!(s.is_open(FlapMode::Overlay));
            s.toggle(FlapMode::Overlay);
            assert!(!s.is_open(FlapMode::Overlay));
        }
        // Phase: crossing_breakpoint_resets_user_preference.
        {
            let mut s = FlapState::open_when_docked();
            s.reconcile(FlapMode::Docked);
            s.toggle(FlapMode::Docked); // user hides while docked
            assert!(!s.is_open(FlapMode::Docked));
            s.reconcile(FlapMode::Overlay); // narrows → preference clears
            assert!(!s.is_open(FlapMode::Overlay));
            s.reconcile(FlapMode::Docked); // widens → back to docked default
            assert!(s.is_open(FlapMode::Docked));
        }
        // Phase: tick_eases_toward_target_and_settles.
        {
            let mut s = FlapState::open_when_docked(); // reveal 1.0
            s.reconcile(FlapMode::Docked); // first resolution: settles open (1.0)
            s.reconcile(FlapMode::Overlay); // genuine crossing: target closed, animates
            assert!(
                s.reveal() > 0.5,
                "a real crossing animates rather than snapping"
            );
            let mut animating = true;
            for _ in 0..600 {
                animating = s.tick_with_dt(FlapMode::Overlay, 0.016);
                if !animating {
                    break;
                }
            }
            assert!(!animating, "transition settles");
            assert!(s.reveal() < 0.01, "settled near closed, got {}", s.reveal());
        }
        // Phase: narrow_first_run_starts_closed_no_flash.
        {
            // A left flap seeded docked-open (reveal 1.0) whose first resolved
            // width is narrow must settle *closed* on frame one — never paint the
            // rail + sidebar and then slide them away.
            let mut s = FlapState::open_when_docked();
            s.reconcile(FlapMode::Overlay);
            assert_eq!(
                s.reveal(),
                0.0,
                "narrow first frame is fully closed, no flash"
            );
            assert!(!s.tick(FlapMode::Overlay), "nothing left to animate");
        }
        // Phase: wide_first_run_starts_open_without_sliding_in.
        {
            let mut s = FlapState::open_when_docked();
            s.reconcile(FlapMode::Docked);
            assert_eq!(
                s.reveal(),
                1.0,
                "desktop first frame shows the rail immediately"
            );
            assert!(!s.tick(FlapMode::Docked), "settled open, no slide-in");
        }
    }
}
