//! Adaptive-chrome constants for the request workspace.
//!
//! The collections / environments / history rail is the app's **primary
//! sidebar**. It docks beside the request builder on a wide window and collapses
//! to a GNOME Adwaita `OverlaySplitView` ("flap") on a narrow one — summoned by
//! the hamburger, sliding in over a dimming scrim. The behaviour itself is the
//! shared [`ui::widget::SidebarDrawer`] (docked-vs-overlay decision, eased
//! reveal, scrim, breakpoint reconciliation); only the app-specific rail width +
//! breakpoint live here so the request editor keeps its own narrower rail.

use ui::widget::DEFAULT_BREAKPOINT;

/// Width breakpoint (logical px) below which the primary rail collapses from
/// docked to overlay. Shared with the rest of the OS via the toolkit default so
/// every adaptive surface flips at the same window size.
pub const RAIL_BREAKPOINT: f64 = DEFAULT_BREAKPOINT;

/// The docked / fully-revealed rail width (logical px). The builder keeps the
/// app's own narrower rail rather than the toolkit's default, so the request
/// editor gets as much room as possible.
pub const RAIL_WIDTH: f64 = 264.0;
