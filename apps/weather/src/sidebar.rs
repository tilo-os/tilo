//! Adaptive sidebar — a GNOME Adwaita `OverlaySplitView` ("flap") in
//! miniature.
//!
//! The saved-places rail behaves two ways depending on how much room the
//! window gives the app, and the choice is made from the widget's
//! *allocated* width (known only after layout), not a fixed config:
//!
//! * **Docked** (wide window): the rail sits permanently beside the
//!   content, which is inset to make room. This is the desktop case.
//! * **Overlay** (narrow window): the content uses the full width and the
//!   rail, when opened, slides in *over* it behind a dimming scrim — exactly
//!   like Adwaita's flap on a phone-width window. It is **closed by
//!   default** at this size so a narrow window opens to the forecast.
//!
//! A user toggle (the hamburger button) flips the rail open/closed; the
//! [`SidebarState`] eases a 0→1 `reveal` value every frame so the slide and
//! scrim fade are smooth. When the window crosses the breakpoint the rail
//! adopts the new mode's sensible default unless the user has expressed an
//! explicit preference for the current session.

use std::time::Instant;

/// Below this allocated content width (logical px) the rail switches from
/// docked to overlay. Chosen so the forecast's 5 metric tiles + the rail
/// stop fitting comfortably side by side.
pub const OVERLAY_BREAKPOINT: f64 = 900.0;

/// Rail width when docked or fully revealed (logical px).
pub const RAIL_W: f64 = 300.0;

/// How the rail relates to the content at the current window size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarMode {
    /// Wide window: rail is permanently beside the (inset) content.
    Docked,
    /// Narrow window: rail overlays the full-width content behind a scrim.
    Overlay,
}

impl SidebarMode {
    /// Decide the mode purely from the allocated width.
    pub fn from_width(total_w: f64) -> Self {
        if total_w >= OVERLAY_BREAKPOINT {
            SidebarMode::Docked
        } else {
            SidebarMode::Overlay
        }
    }
}

/// The animated open/closed state of the rail, plus the user's explicit
/// preference (if any) for this session.
#[derive(Clone, Debug)]
pub struct SidebarState {
    /// `Some(true/false)` once the user has toggled; `None` means "follow
    /// the mode's default" (docked ⇒ open, overlay ⇒ closed).
    user_open: Option<bool>,
    /// Eased reveal amount: 0 = fully closed, 1 = fully open. Drives the
    /// slide offset and scrim opacity so transitions are smooth.
    reveal: f64,
    /// Last mode we resolved, so we can detect breakpoint crossings and
    /// reset a stale user preference to the new default.
    last_mode: Option<SidebarMode>,
    /// Wall-clock of the last animation tick.
    last_tick: Instant,
}

impl Default for SidebarState {
    fn default() -> Self {
        Self {
            user_open: None,
            // Start docked-open so a desktop-width first frame (and `--shots`,
            // which renders at 1280px) shows the rail without animating in.
            reveal: 1.0,
            last_mode: None,
            last_tick: Instant::now(),
        }
    }
}

impl SidebarState {
    /// Whether the rail should currently be open in `mode`, honouring the
    /// user's preference and falling back to the mode default.
    pub fn is_open(&self, mode: SidebarMode) -> bool {
        self.user_open.unwrap_or(match mode {
            SidebarMode::Docked => true,
            SidebarMode::Overlay => false,
        })
    }

    /// Flip the user preference relative to the current resolved openness.
    pub fn toggle(&mut self, mode: SidebarMode) {
        let now_open = self.is_open(mode);
        self.user_open = Some(!now_open);
    }

    /// Force the rail closed (e.g. tapping the scrim in overlay mode).
    pub fn close(&mut self) {
        self.user_open = Some(false);
    }

    /// Reconcile with the latest mode: on a breakpoint crossing, drop any
    /// stale user preference so the rail adopts the new mode's natural
    /// default (wide ⇒ shown, narrow ⇒ hidden). Call once per frame with the
    /// freshly-computed mode before reading [`Self::reveal`].
    pub fn reconcile(&mut self, mode: SidebarMode) {
        if self.last_mode != Some(mode) {
            if self.last_mode.is_some() {
                // Crossed the breakpoint — return to auto behaviour.
                self.user_open = None;
            } else {
                // First mode ever resolved (the real width is only known
                // after layout). Snap the reveal to this mode's resting
                // state so a narrow-first window opens already-collapsed
                // instead of flashing the rail open then animating it shut.
                self.reveal = if self.is_open(mode) { 1.0 } else { 0.0 };
            }
            self.last_mode = Some(mode);
        }
    }

    /// Advance the eased reveal toward the target openness using real
    /// elapsed time (frame-rate independent). Returns true while still
    /// animating, so the widget can request another frame.
    pub fn tick(&mut self, mode: SidebarMode) -> bool {
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f64().min(0.1);
        self.last_tick = now;
        self.tick_with_dt(mode, dt)
    }

    /// The frame-rate-independent core of [`Self::tick`], split out so the
    /// easing is deterministically testable with an explicit `dt`.
    fn tick_with_dt(&mut self, mode: SidebarMode, dt: f64) -> bool {
        let target = if self.is_open(mode) { 1.0 } else { 0.0 };
        // Exponential approach: covers ~63% of the remaining gap every
        // `1/rate` seconds. rate≈14 gives a crisp ~0.2s Adwaita-like slide.
        let rate = 14.0;
        let alpha = 1.0 - (-rate * dt).exp();
        self.reveal += (target - self.reveal) * alpha;
        if (self.reveal - target).abs() < 0.001 {
            self.reveal = target;
            false
        } else {
            true
        }
    }

    /// Current eased reveal amount in `0.0..=1.0`.
    pub fn reveal(&self) -> f64 {
        self.reveal.clamp(0.0, 1.0)
    }

    /// Eased reveal smoothed once more for the slide offset, so the rail
    /// decelerates into place (an ease-out feel on top of the exponential).
    pub fn slide(&self) -> f64 {
        let r = self.reveal();
        // ease-out cubic
        1.0 - (1.0 - r).powi(3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_switches_at_breakpoint() {
        assert_eq!(SidebarMode::from_width(1280.0), SidebarMode::Docked);
        assert_eq!(SidebarMode::from_width(700.0), SidebarMode::Overlay);
        assert_eq!(SidebarMode::from_width(OVERLAY_BREAKPOINT), SidebarMode::Docked);
        assert_eq!(SidebarMode::from_width(OVERLAY_BREAKPOINT - 1.0), SidebarMode::Overlay);
    }

    #[test]
    fn defaults_open_when_docked_closed_when_overlay() {
        let s = SidebarState::default();
        assert!(s.is_open(SidebarMode::Docked), "wide window shows the rail");
        assert!(!s.is_open(SidebarMode::Overlay), "narrow window hides it by default");
    }

    #[test]
    fn toggle_flips_relative_to_current() {
        let mut s = SidebarState::default();
        // Overlay default is closed → toggle opens.
        s.toggle(SidebarMode::Overlay);
        assert!(s.is_open(SidebarMode::Overlay));
        s.toggle(SidebarMode::Overlay);
        assert!(!s.is_open(SidebarMode::Overlay));
    }

    #[test]
    fn crossing_breakpoint_resets_user_preference() {
        let mut s = SidebarState::default();
        s.reconcile(SidebarMode::Docked);
        // User hides the rail while docked.
        s.toggle(SidebarMode::Docked);
        assert!(!s.is_open(SidebarMode::Docked));
        // Window narrows past the breakpoint → preference clears, overlay
        // default (closed) applies; widening again returns to docked-open.
        s.reconcile(SidebarMode::Overlay);
        assert!(!s.is_open(SidebarMode::Overlay));
        s.reconcile(SidebarMode::Docked);
        assert!(s.is_open(SidebarMode::Docked), "back to docked default = open");
    }

    #[test]
    fn tick_eases_toward_target_and_settles() {
        let mut s = SidebarState::default(); // reveal starts at 1.0
        s.reconcile(SidebarMode::Overlay); // target closed (0)
        // Step with a realistic ~16ms frame dt until settled (or give up).
        let mut animating = true;
        for _ in 0..600 {
            animating = s.tick_with_dt(SidebarMode::Overlay, 0.016);
            if !animating {
                break;
            }
        }
        assert!(!animating, "transition eventually settles");
        assert!(s.reveal() < 0.01, "settled near closed, got {}", s.reveal());

        // And it monotonically approaches the open target from closed.
        let mut o = SidebarState { user_open: Some(true), reveal: 0.0, last_mode: Some(SidebarMode::Overlay), last_tick: Instant::now() };
        let r0 = o.reveal();
        o.tick_with_dt(SidebarMode::Overlay, 0.016);
        assert!(o.reveal() > r0, "reveal increases toward open");
    }
}
