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
//! The pane state and eased slide are the toolkit's [`ui::widget::PushNav`], so
//! the transition matches the rest of the OS; this module owns only the width
//! decision.

use ui::widget::SplitLayout;

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
}
