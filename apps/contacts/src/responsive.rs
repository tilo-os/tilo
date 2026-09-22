//! Width-driven responsiveness for Contacts.
//!
//! Contacts is a master-detail layout: a searchable, sortable contact
//! **list** (the master) beside a **detail pane** that shows the selected
//! person's card or the edit form. On a desktop-width window both panes sit
//! side by side with a draggable divider. That stops working once the
//! window is narrow enough that neither pane has room to breathe — so below
//! a width breakpoint the address book switches to single-pane **push
//! navigation**: it shows the list OR the detail, and selecting a contact
//! pushes the detail in (with a Back affordance) over an eased slide.
//! Returning Back (or Esc) slides the list back.
//!
//! The breakpoint is decided from the app's *allocated* width, not a fixed
//! config, and the eased slide is the shared toolkit kernel
//! ([`ui::widget::FlapState`]) — the exact algorithm proven in the weather
//! and chat flaps — so the transition matches the rest of the OS.

use std::time::Instant;

use ui::widget::{FlapState, SplitLayout};

/// Below this allocated width (logical px) the list+detail split collapses
/// into single-pane push navigation. Chosen so the contact list and a
/// readable detail/edit pane stop fitting comfortably side by side (the
/// detail form's two-column rows need ~360px and the list ~280px plus the
/// divider).
pub const PUSH_BREAKPOINT: f64 = 760.0;

/// Which pane the narrow, single-pane push flow is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    /// The contact list fills the content area.
    List,
    /// The detail / edit pane fills the content area, with a Back affordance.
    Detail,
}

/// Drives the master↔detail push transition on narrow windows.
///
/// Wraps the shared [`FlapState`] kernel: a `reveal` of `0` means the list
/// (master) is shown and `1` means the detail is fully pushed in. The
/// eased `slide` is fed into the layout so the panes decelerate into place.
/// On wide windows this state is inert — both panes are always laid out
/// side by side and `reveal` is irrelevant.
pub struct PushNav {
    /// Which pane is the *target* of the current transition.
    pane: Pane,
    /// The eased slide kernel. `reveal == 1` ⇒ detail fully shown.
    flap: FlapState,
    /// Wall-clock of the last animation tick (the toolkit kernel ticks on
    /// an explicit `dt`, so we keep our own clock).
    last_tick: Instant,
}

impl Default for PushNav {
    fn default() -> Self {
        Self {
            pane: Pane::List,
            // Closed by default ⇒ reveal starts at 0 ⇒ a narrow first frame
            // (and `--shots`) opens to the list, matching the master-detail
            // convention.
            flap: FlapState::closed_by_default(),
            last_tick: Instant::now(),
        }
    }
}

impl PushNav {
    /// Resolve the split layout purely from the allocated width.
    pub fn layout(total_w: f64) -> SplitLayout {
        Self::layout_at(total_w, 1.0)
    }

    /// Resolve the split layout for a window of `total_w` logical pixels at
    /// the theme's `text_scale`. The breakpoint tracks the scale because the
    /// list and detail columns are sized by the text they hold: at 2× a
    /// 1200px window has the room a 600px window has at 1×, so it pushes.
    pub fn layout_at(total_w: f64, text_scale: f32) -> SplitLayout {
        SplitLayout::from_width(total_w, PUSH_BREAKPOINT * f64::from(text_scale.max(1.0)))
    }

    /// `true` when the window is narrow enough for single-pane push.
    pub fn is_narrow(total_w: f64) -> bool {
        Self::layout(total_w) == SplitLayout::Overlay
    }

    /// [`Self::is_narrow`] at the theme's text scale.
    pub fn is_narrow_at(total_w: f64, text_scale: f32) -> bool {
        Self::layout_at(total_w, text_scale) == SplitLayout::Overlay
    }

    /// Which pane the push flow currently targets.
    pub fn pane(&self) -> Pane {
        self.pane
    }

    /// Push the detail pane in (call from a contact-selection handler).
    pub fn show_detail(&mut self) {
        self.pane = Pane::Detail;
        self.flap.open();
    }

    /// Slide back to the list (the Back affordance / Esc).
    pub fn show_list(&mut self) {
        self.pane = Pane::List;
        self.flap.close();
    }

    /// The eased slide in `0.0..=1.0`: `0` = list shown, `1` = detail shown.
    /// Fed straight into the layout to position the two single-pane slabs.
    pub fn slide(&self) -> f64 {
        self.flap.slide()
    }

    /// Advance the slide animation toward its target. Returns `true` while
    /// still animating, so the host can request another frame.
    pub fn tick(&mut self, reduce_motion: bool) -> bool {
        if reduce_motion {
            self.flap.snap(self.pane == Pane::Detail);
            self.last_tick = Instant::now();
            return false;
        }
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f64().min(0.1);
        self.last_tick = now;
        // The list/detail target is a binary open/closed flap; the
        // `SplitLayout` argument only selects the default for the *layout*
        // mode, and here the target is always driven explicitly via
        // `open`/`close`, so `Overlay` is the right context to pass.
        self.flap.tick(SplitLayout::Overlay, dt)
    }

    /// `true` while the push slide hasn't settled.
    pub fn is_animating(&self) -> bool {
        self.flap.is_animating(SplitLayout::Overlay)
    }

    /// Snap the slide to its endpoint with no animation — used by `--shots`
    /// so a headless frame captures a settled pane rather than one
    /// mid-slide.
    pub fn snap(&mut self, pane: Pane) {
        self.pane = pane;
        self.flap.snap(pane == Pane::Detail);
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;

    #[test]
    fn wide_window_is_docked_narrow_is_push() {
        assert_eq!(PushNav::layout(1280.0), SplitLayout::Docked);
        assert_eq!(PushNav::layout(700.0), SplitLayout::Overlay);
        // Boundary is inclusive at the breakpoint → still docked.
        assert_eq!(PushNav::layout(PUSH_BREAKPOINT), SplitLayout::Docked);
        assert_eq!(PushNav::layout(PUSH_BREAKPOINT - 1.0), SplitLayout::Overlay);
        assert!(!PushNav::is_narrow(1280.0));
        assert!(PushNav::is_narrow(700.0));
        // At 2× the same 1280px window has the room a 640px window has at 1×,
        // so it pushes; a scale below 1× never lowers the 1× breakpoint.
        assert!(PushNav::is_narrow_at(1280.0, 2.0));
        assert!(!PushNav::is_narrow_at(2.0 * PUSH_BREAKPOINT, 2.0));
        assert_eq!(
            PushNav::layout_at(PUSH_BREAKPOINT, 0.5),
            SplitLayout::Docked
        );
        assert_eq!(
            PushNav::layout_at(PUSH_BREAKPOINT - 1.0, 0.5),
            SplitLayout::Overlay
        );
    }

    #[test]
    fn defaults_to_the_list_pane() {
        let nav = PushNav::default();
        assert_eq!(nav.pane(), Pane::List);
        assert_eq!(nav.slide(), 0.0, "a fresh narrow frame opens to the list");
    }

    #[test]
    fn selecting_pushes_detail_back_returns_to_list() {
        let mut nav = PushNav::default();
        nav.show_detail();
        nav.snap(Pane::Detail);
        assert_eq!(nav.pane(), Pane::Detail);
        assert_eq!(nav.slide(), 1.0, "detail fully shown after a selection");
        nav.show_list();
        nav.snap(Pane::List);
        assert_eq!(nav.pane(), Pane::List);
        assert_eq!(nav.slide(), 0.0, "Back returns to the list");
    }

    #[test]
    fn snap_settles_without_animating() {
        let mut nav = PushNav::default();
        nav.show_detail();
        nav.snap(Pane::Detail);
        assert!(!nav.is_animating(), "snap leaves nothing to animate");
    }

    #[test]
    fn reduced_motion_lands_on_the_target_without_requesting_frames() {
        let mut nav = PushNav::default();
        nav.show_detail();
        assert!(nav.is_animating());
        assert!(!nav.tick(true));
        assert_eq!(nav.slide(), 1.0);
        assert!(!nav.is_animating());

        nav.show_list();
        assert!(!nav.tick(true));
        assert_eq!(nav.slide(), 0.0);
        assert!(!nav.is_animating());
    }
}
