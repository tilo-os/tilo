//! Adaptive chrome for the request workspace — a GNOME Adwaita
//! `OverlaySplitView` ("flap") driven by the window's allocated width.
//!
//! The collections / environments / history rail is the app's **primary
//! sidebar**. It behaves two ways depending on how wide the window is, and
//! the choice is made purely from the widget's allocated width — never a
//! fixed config:
//!
//! * **Docked** (wide window): the rail sits permanently beside the request
//!   builder, which is inset to make room. The desktop case, unchanged from
//!   the original layout.
//! * **Overlay** (narrow window): the builder uses the full width and the
//!   rail, when summoned by the hamburger toggle, slides in *over* it behind
//!   a dimming scrim — exactly like Adwaita's flap on a phone-width window. It
//!   is **closed by default** at this size so a narrow window opens straight
//!   to the request builder.
//!
//! The app is a single self-drawing canvas (it paints the rail and the
//! builder into one Vello scene and hit-tests sub-regions by hand), so it
//! adopts the shared [`FlapState`] easing kernel from the toolkit to drive
//! its existing rail offset + scrim rather than composing two independent
//! widgets through [`ui::widget::NavSplit`]. The kernel — a frame-rate
//! independent exponential approach with an ease-out-cubic slide — is the
//! exact algorithm `NavSplit` itself uses, so the rail animates identically
//! to every other adaptive surface in the OS.

use ui::widget::{FlapState, SplitLayout, DEFAULT_BREAKPOINT};

/// Width breakpoint (logical px) below which the primary rail collapses from
/// docked to overlay. Shared with the rest of the OS via the toolkit default
/// so every adaptive surface flips at the same window size.
pub const RAIL_BREAKPOINT: f64 = DEFAULT_BREAKPOINT;

/// The docked / fully-revealed rail width (logical px). The builder keeps the
/// app's own narrower rail rather than the toolkit's default, so the request
/// editor gets as much room as possible.
pub const RAIL_WIDTH: f64 = 264.0;

/// The animated open/closed state of the primary rail, plus its docked/overlay
/// resolution. A thin wrapper over the shared [`FlapState`] kernel that tracks
/// the last layout so the host can drive the per-frame tick with real elapsed
/// time.
#[derive(Clone, Debug)]
pub struct RailState {
    flap: FlapState,
    /// Last resolved layout, exposed so the host can react to a docked⇄overlay
    /// crossing (e.g. to clear a stale "rail hidden" preference).
    layout: SplitLayout,
    /// Whether the first width-resolve has happened yet. The reveal is seeded
    /// for a docked first frame; if the very first frame is actually narrow we
    /// snap to that layout's default instead of animating in from a phantom
    /// prior state.
    settled: bool,
}

impl Default for RailState {
    fn default() -> Self {
        Self {
            // Open when docked so a desktop-width first frame (and the wide
            // `--shots`, 1280px) shows the rail without animating in.
            flap: FlapState::open_when_docked(),
            layout: SplitLayout::Docked,
            settled: false,
        }
    }
}

impl RailState {
    /// Resolve docked vs. overlay from the allocated width. The boundary is
    /// inclusive at the breakpoint → docked, matching every other adaptive
    /// surface in the OS.
    pub fn layout_for(width: f64) -> SplitLayout {
        SplitLayout::from_width(width, RAIL_BREAKPOINT)
    }

    /// Reconcile + cache the layout for the current width. On a breakpoint
    /// crossing the shared kernel drops any stale user preference so the rail
    /// returns to the new layout's default (wide ⇒ shown, narrow ⇒ hidden).
    /// Call once per frame before reading [`Self::reveal`] / [`Self::slide`].
    pub fn resolve(&mut self, width: f64) -> SplitLayout {
        let layout = Self::layout_for(width);
        self.flap.reconcile(layout);
        self.layout = layout;
        // The default reveal is seeded for a docked first frame (1.0). If the
        // very first frame is actually narrow there is no prior on-screen state
        // to animate from, so snap the reveal to that layout's resolved
        // openness rather than sliding in from the stale seed. This keeps both
        // single-frame `--shots` and the live first paint settled. A docked
        // first frame already matches the seed, so it's left untouched (its
        // "follow the default" preference stays implicit).
        if !self.settled {
            if layout == SplitLayout::Overlay {
                self.flap.snap(self.flap.is_open(layout));
            }
            self.settled = true;
        }
        layout
    }

    /// The last resolved layout (docked or overlay).
    pub fn layout(&self) -> SplitLayout {
        self.layout
    }

    /// Whether the rail is currently open (resolved against the layout
    /// default). Docked ⇒ open unless hidden; overlay ⇒ closed unless summoned.
    pub fn is_open(&self) -> bool {
        self.flap.is_open(self.layout)
    }

    /// Flip the rail open/closed relative to its current resolved openness —
    /// the hamburger toggle.
    pub fn toggle(&mut self) {
        self.flap.toggle(self.layout);
    }

    /// Force the rail closed (scrim click / Esc in overlay mode).
    pub fn close(&mut self) {
        self.flap.close();
    }

    /// Set the rail open/closed with NO animation — snaps the reveal to its
    /// endpoint. Used by `--shots` so a single offscreen frame captures a
    /// settled rail rather than one mid-slide.
    pub fn snap(&mut self, open: bool) {
        self.flap.snap(open);
    }

    /// Advance the eased reveal by `dt` seconds toward the current target.
    /// Returns `true` while still animating, so the host can keep pumping
    /// redraws until the slide settles.
    pub fn tick(&mut self, dt: f64) -> bool {
        self.flap.tick(self.layout, dt)
    }

    /// `true` while the slide/scrim transition hasn't settled.
    pub fn is_animating(&self) -> bool {
        self.flap.is_animating(self.layout)
    }

    /// Raw eased reveal in `0..=1` — drives the docked content inset and the
    /// overlay scrim opacity.
    pub fn reveal(&self) -> f64 {
        self.flap.reveal()
    }

    /// Ease-out-cubic of the reveal — drives the overlay slide offset so the
    /// rail decelerates into place.
    pub fn slide(&self) -> f64 {
        self.flap.slide()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::DEFAULT_SIDEBAR_WIDTH;

    #[test]
    fn wide_resolves_docked_open_by_default() {
        let mut s = RailState::default();
        let layout = s.resolve(1280.0);
        assert_eq!(layout, SplitLayout::Docked);
        assert!(s.is_open(), "a docked rail is open by default");
    }

    #[test]
    fn narrow_resolves_overlay_closed_by_default() {
        let mut s = RailState::default();
        let layout = s.resolve(700.0);
        assert_eq!(layout, SplitLayout::Overlay);
        assert!(!s.is_open(), "a narrow rail starts collapsed");
    }

    #[test]
    fn breakpoint_boundary_is_inclusive_docked() {
        assert_eq!(RailState::layout_for(RAIL_BREAKPOINT), SplitLayout::Docked);
        assert_eq!(
            RailState::layout_for(RAIL_BREAKPOINT - 1.0),
            SplitLayout::Overlay
        );
    }

    #[test]
    fn hamburger_toggles_overlay_rail() {
        let mut s = RailState::default();
        s.resolve(700.0);
        assert!(!s.is_open());
        s.toggle();
        assert!(s.is_open(), "the hamburger summons the overlay rail");
        s.toggle();
        assert!(!s.is_open(), "toggling again dismisses it");
    }

    #[test]
    fn scrim_close_dismisses_summoned_rail() {
        let mut s = RailState::default();
        s.resolve(700.0);
        s.toggle();
        assert!(s.is_open());
        s.close();
        assert!(!s.is_open(), "scrim / Esc closes the overlay rail");
    }

    #[test]
    fn snap_open_settles_reveal_without_animating() {
        let mut s = RailState::default();
        s.resolve(700.0);
        s.snap(true);
        assert!(s.is_open());
        assert!((s.reveal() - 1.0).abs() < 1e-9, "snapped fully open");
        assert!(!s.is_animating(), "snap leaves nothing to animate");
    }

    #[test]
    fn crossing_breakpoint_restores_layout_default() {
        let mut s = RailState::default();
        s.resolve(1280.0); // docked
        s.toggle(); // user hides the rail while docked
        assert!(!s.is_open());
        s.resolve(700.0); // narrow → preference clears, overlay default closed
        assert!(!s.is_open());
        s.resolve(1280.0); // widen → back to docked-open default
        assert!(s.is_open(), "widening past the breakpoint reopens the rail");
    }

    #[test]
    fn docked_default_width_constant_is_not_used_directly() {
        // The app keeps its own narrower rail; assert the toolkit default is
        // wider so this is a deliberate override, not an accidental shadow.
        assert!(RAIL_WIDTH < DEFAULT_SIDEBAR_WIDTH);
    }
}
