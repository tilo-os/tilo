//! Width-driven responsiveness for the Logs viewer.
//!
//! Logs is a master-detail layout: a severity-coloured entry **list**
//! (the master) beside a **detail pane** that surfaces every field of the
//! selected record. On a desktop-width window both panes sit side by side.
//! That stops working once the window is narrow enough that neither pane
//! has room to breathe — so below a width breakpoint the viewer switches
//! to single-pane **push navigation** ([`ui::widget::PushNav`]): it shows the
//! list OR the detail, and selecting a row pushes the detail in (with a Back
//! affordance) over an eased slide. Returning Back (or Esc) slides the list
//! back.
//!
//! The breakpoint is decided from the app's *allocated* width, not a fixed
//! config.

use ui::widget::SplitLayout;

/// Below this allocated width (logical px) the list+detail split collapses
/// into single-pane push navigation. Chosen so the entry list's columns
/// (time · level · unit · message) plus a readable detail pane stop
/// fitting comfortably side by side.
pub const PUSH_BREAKPOINT: f64 = 760.0;

/// Resolve the split layout purely from the allocated width.
pub fn layout(total_w: f64) -> SplitLayout {
    SplitLayout::from_width(total_w, PUSH_BREAKPOINT)
}

/// `true` when the window is narrow enough for single-pane push.
pub fn is_narrow(total_w: f64) -> bool {
    layout(total_w) == SplitLayout::Overlay
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_window_is_docked_narrow_is_push() {
        assert_eq!(layout(1280.0), SplitLayout::Docked);
        assert_eq!(layout(700.0), SplitLayout::Overlay);
        // Boundary is inclusive at the breakpoint → still docked.
        assert_eq!(layout(PUSH_BREAKPOINT), SplitLayout::Docked);
        assert_eq!(layout(PUSH_BREAKPOINT - 1.0), SplitLayout::Overlay);
        assert!(!is_narrow(1280.0));
        assert!(is_narrow(700.0));
    }
}
