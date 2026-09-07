//! Width-driven responsiveness for the Monitor.
//!
//! The system stats are **content**, not navigation, so the Monitor has no
//! sidebar at any width. Instead each tab reflows in place: the Processes tab
//! wraps its compact top summary strip, and the Resources dashboard reflows
//! its overview / per-core / chart bands (see [`crate::layout`]).
//!
//! The one place a *width* still switches a whole layout is the Processes
//! tab's master-detail relationship. Wide, the process **table** and the
//! selected-process **detail** strip stack together. Below the narrow
//! breakpoint the tab becomes single-pane **push navigation**: it shows the
//! table OR the detail, and selecting a row pushes the detail in (with a Back
//! affordance) over an eased slide. Returning Back (or Esc) slides the table
//! back — the content-panel behaviour the HIG mandates for narrow windows.
//!
//! The push eases with the shared toolkit kernel ([`ui::widget::FlapState`]) —
//! the same algorithm proven in the weather and chat flaps — so the transition
//! matches the rest of the OS.

use std::time::Instant;

use ui::widget::{FlapState, SplitLayout};

/// At or below this allocated width (logical px) the Processes tab collapses
/// its table+detail into single-pane push navigation. Chosen so a full-width
/// process table plus its bottom detail strip stop fitting comfortably
/// together and the detail earns its own pushed pane instead.
pub const NARROW_BREAKPOINT: f64 = 680.0;

/// Resolve the split layout purely from the allocated width. The boundary is
/// inclusive at the breakpoint → [`SplitLayout::Docked`].
pub fn layout_for(total_w: f64) -> SplitLayout {
    SplitLayout::from_width(total_w, NARROW_BREAKPOINT)
}

/// `true` when the window is narrow enough for the single-pane push flow.
pub fn is_narrow(total_w: f64) -> bool {
    layout_for(total_w) == SplitLayout::Overlay
}

/// Which pane the narrow, single-pane Processes push flow is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcPane {
    /// The process table fills the content area.
    Table,
    /// The process detail fills the content area, with a Back affordance.
    Detail,
}

/// Drives the Processes table↔detail push transition on narrow windows.
///
/// Wraps the shared [`FlapState`] kernel: a `slide` of `0` means the table
/// (master) is shown and `1` means the detail is fully pushed in. On wide
/// windows this state is inert — the table and detail are always laid out
/// together (table above the detail strip) and `slide` is irrelevant.
pub struct ProcPush {
    pane: ProcPane,
    flap: FlapState,
    last_tick: Instant,
}

impl Default for ProcPush {
    fn default() -> Self {
        Self {
            pane: ProcPane::Table,
            // Closed by default ⇒ slide starts at 0 ⇒ a narrow first frame
            // (and `--shots`) opens to the table, matching the master-detail
            // convention.
            flap: FlapState::closed_by_default(),
            last_tick: Instant::now(),
        }
    }
}

impl ProcPush {
    /// Which pane the push flow currently targets.
    pub fn pane(&self) -> ProcPane {
        self.pane
    }

    /// Push the detail pane in (call from a row-selection handler).
    pub fn show_detail(&mut self) {
        self.pane = ProcPane::Detail;
        self.flap.open();
    }

    /// Slide back to the table (the Back affordance / Esc).
    pub fn show_table(&mut self) {
        self.pane = ProcPane::Table;
        self.flap.close();
    }

    /// The eased slide in `0.0..=1.0`: `0` = table shown, `1` = detail shown.
    pub fn slide(&self) -> f64 {
        self.flap.slide()
    }

    /// Advance the slide toward its target. Returns `true` while still
    /// animating, so the host can request another frame.
    pub fn tick(&mut self, reduce_motion: bool) -> bool {
        if reduce_motion {
            self.snap(self.pane);
            return false;
        }
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f64().min(0.1);
        self.last_tick = now;
        // The table/detail target is a binary open/closed flap driven
        // explicitly via `open`/`close`; the `SplitLayout` argument only
        // selects the *layout* default, so `Overlay` is the right context.
        self.flap.tick(SplitLayout::Overlay, dt)
    }

    /// `true` while the push slide hasn't settled.
    pub fn is_animating(&self) -> bool {
        self.flap.is_animating(SplitLayout::Overlay)
    }

    /// Snap the slide to its endpoint with no animation — used by `--shots`.
    pub fn snap(&mut self, pane: ProcPane) {
        self.pane = pane;
        self.flap.snap(pane == ProcPane::Detail);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_is_docked_narrow_is_overlay() {
        // A desktop and a medium (800px) window keep the stacked table+detail.
        assert_eq!(layout_for(1280.0), SplitLayout::Docked);
        assert_eq!(layout_for(800.0), SplitLayout::Docked);
        // A phone-narrow window switches to single-pane push.
        assert_eq!(layout_for(560.0), SplitLayout::Overlay);
        // Boundary is inclusive at the breakpoint → still docked.
        assert_eq!(layout_for(NARROW_BREAKPOINT), SplitLayout::Docked);
        assert_eq!(layout_for(NARROW_BREAKPOINT - 1.0), SplitLayout::Overlay);
        assert!(!is_narrow(1280.0));
        assert!(!is_narrow(800.0));
        assert!(is_narrow(560.0));
    }

    #[test]
    fn proc_push_defaults_to_table() {
        let p = ProcPush::default();
        assert_eq!(p.pane(), ProcPane::Table);
        assert_eq!(p.slide(), 0.0, "a fresh narrow frame opens to the table");
    }

    #[test]
    fn proc_push_selecting_pushes_detail_back_returns() {
        let mut p = ProcPush::default();
        p.show_detail();
        p.snap(ProcPane::Detail);
        assert_eq!(p.pane(), ProcPane::Detail);
        assert_eq!(p.slide(), 1.0, "detail fully shown after a selection");
        p.show_table();
        p.snap(ProcPane::Table);
        assert_eq!(p.pane(), ProcPane::Table);
        assert_eq!(p.slide(), 0.0, "Back returns to the table");
    }

    #[test]
    fn proc_push_snap_settles_without_animating() {
        let mut p = ProcPush::default();
        p.show_detail();
        p.snap(ProcPane::Detail);
        assert!(!p.is_animating(), "snap leaves nothing to animate");
    }
}
