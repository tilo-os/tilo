//! Width-driven responsiveness for the Monitor.
//!
//! Monitor pairs an always-present **system rail** (CPU ring, memory, net /
//! disk sparklines, host info) with a tabbed content area. On a desktop-width
//! window the rail sits permanently beside the content. That stops working
//! once the window is narrow enough that the rail and the content's table or
//! chart grid can no longer both breathe — so below a width breakpoint the
//! rail collapses into an **overlay flap**: closed by default, summoned by a
//! hamburger toggle, sliding in over the content behind a dimming scrim and
//! dismissed by the scrim, the Esc key, or the toggle.
//!
//! The Processes tab is itself a master-detail layout (a process **table**
//! plus a selected-process **detail** strip). Below the same breakpoint it
//! additionally switches to single-pane **push navigation**: it shows the
//! table OR the detail, and selecting a row pushes the detail in (with a Back
//! affordance) over an eased slide. Returning Back (or Esc) slides the table
//! back.
//!
//! Both behaviours are driven by the app's *allocated* width, not a fixed
//! config, and both ease with the shared toolkit kernel
//! ([`ui::widget::FlapState`]) — the exact algorithm proven in the weather and
//! chat flaps — so the transitions match the rest of the OS.

use std::time::Instant;

use ui::widget::{FlapState, SplitLayout};

/// Below this allocated width (logical px) the rail collapses to an overlay
/// flap and the Processes table+detail collapses to single-pane push. Chosen
/// so the 248px rail plus the process table's six columns (or the Resources
/// 2×2 chart grid) stop fitting comfortably side by side.
pub const NARROW_BREAKPOINT: f64 = 820.0;

/// Resolve the split layout purely from the allocated width. The boundary is
/// inclusive at the breakpoint → [`SplitLayout::Docked`].
pub fn layout_for(total_w: f64) -> SplitLayout {
    SplitLayout::from_width(total_w, NARROW_BREAKPOINT)
}

/// `true` when the window is narrow enough for the overlay rail + push flow.
pub fn is_narrow(total_w: f64) -> bool {
    layout_for(total_w) == SplitLayout::Overlay
}

/// The animated open/closed state of the system rail.
///
/// Wraps the shared [`FlapState`] kernel configured as a navigation sidebar:
/// **open** when the window is docked-wide, **closed** when it collapses to an
/// overlay. The eased `reveal` drives both the docked content inset and the
/// overlay slide / scrim opacity so transitions are smooth.
pub struct RailFlap {
    flap: FlapState,
    last_tick: Instant,
    /// `true` until the first layout has been resolved. The flap's eased
    /// `reveal` is constructed for the docked default (open); on the first
    /// frame we snap it to whatever the *resolved* width actually wants, so a
    /// window that opens narrow shows a closed rail without animating in (and
    /// a single headless `--shots` frame is correct).
    needs_initial_snap: bool,
}

impl Default for RailFlap {
    fn default() -> Self {
        Self {
            // Open when docked so a desktop-width first frame (and `--shots`,
            // which renders at 1280px) shows the rail without animating in.
            flap: FlapState::open_when_docked(),
            last_tick: Instant::now(),
            needs_initial_snap: true,
        }
    }
}

impl RailFlap {
    /// Resolve the rail's layout from the allocated width.
    pub fn layout(total_w: f64) -> SplitLayout {
        layout_for(total_w)
    }

    /// Whether the rail is currently open at the given layout.
    pub fn is_open(&self, layout: SplitLayout) -> bool {
        self.flap.is_open(layout)
    }

    /// Flip the rail open/closed relative to its current openness.
    pub fn toggle(&mut self, layout: SplitLayout) {
        self.flap.toggle(layout);
    }

    /// Force the rail closed (scrim tap / Esc / row selection in overlay).
    pub fn close(&mut self) {
        self.flap.close();
    }

    /// Drop a stale user preference when the window crosses the breakpoint, so
    /// a resize returns to the auto default (wide ⇒ shown, narrow ⇒ hidden).
    /// On the very first call, settle the eased reveal at the resolved default
    /// so a narrow-first window opens to a closed rail rather than animating
    /// one out (and a single headless `--shots` frame is correct). The settle
    /// runs the shared easing kernel to its endpoint *without* recording a user
    /// preference, so a later resize still returns to auto behaviour. Call once
    /// per frame before reading [`Self::reveal`].
    pub fn reconcile(&mut self, layout: SplitLayout) {
        self.flap.reconcile(layout);
        if self.needs_initial_snap {
            self.needs_initial_snap = false;
            // No user preference yet ⇒ `is_open` follows the layout default;
            // run the kernel to its settling point so `reveal` matches it.
            while self.flap.tick(layout, 0.1) {}
        }
    }

    /// Current eased reveal in `0.0..=1.0` — drives the docked content inset
    /// and the overlay scrim opacity.
    pub fn reveal(&self) -> f64 {
        self.flap.reveal()
    }

    /// Ease-out-cubic slide in `0.0..=1.0` — drives the overlay rail's
    /// off-screen→docked position so it decelerates into place.
    pub fn slide(&self) -> f64 {
        self.flap.slide()
    }

    /// Advance the reveal animation toward its target. Returns `true` while
    /// still animating, so the host can request another frame.
    pub fn tick(&mut self, layout: SplitLayout) -> bool {
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f64().min(0.1);
        self.last_tick = now;
        self.flap.tick(layout, dt)
    }

    /// `true` while the reveal hasn't settled at the given layout.
    pub fn is_animating(&self, layout: SplitLayout) -> bool {
        self.flap.is_animating(layout)
    }

    /// Snap the rail open/closed with no animation — used by `--shots` so a
    /// headless frame captures a settled rail rather than one mid-slide.
    pub fn snap(&mut self, open: bool) {
        self.flap.snap(open);
    }
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
    pub fn tick(&mut self) -> bool {
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
        assert_eq!(layout_for(1280.0), SplitLayout::Docked);
        assert_eq!(layout_for(700.0), SplitLayout::Overlay);
        // Boundary is inclusive at the breakpoint → still docked.
        assert_eq!(layout_for(NARROW_BREAKPOINT), SplitLayout::Docked);
        assert_eq!(layout_for(NARROW_BREAKPOINT - 1.0), SplitLayout::Overlay);
        assert!(!is_narrow(1280.0));
        assert!(is_narrow(700.0));
    }

    #[test]
    fn rail_open_when_docked_closed_when_overlay() {
        let r = RailFlap::default();
        assert!(r.is_open(SplitLayout::Docked), "wide window shows the rail");
        assert!(!r.is_open(SplitLayout::Overlay), "narrow window hides it by default");
    }

    #[test]
    fn rail_toggle_flips_relative_to_current() {
        let mut r = RailFlap::default();
        // Overlay default is closed → toggle opens.
        r.toggle(SplitLayout::Overlay);
        assert!(r.is_open(SplitLayout::Overlay));
        r.toggle(SplitLayout::Overlay);
        assert!(!r.is_open(SplitLayout::Overlay));
    }

    #[test]
    fn rail_crossing_breakpoint_resets_preference() {
        let mut r = RailFlap::default();
        r.reconcile(SplitLayout::Docked);
        // User hides the rail while docked.
        r.toggle(SplitLayout::Docked);
        assert!(!r.is_open(SplitLayout::Docked));
        // Narrow past the breakpoint → preference clears, overlay default
        // (closed) applies; widening again returns to docked-open.
        r.reconcile(SplitLayout::Overlay);
        assert!(!r.is_open(SplitLayout::Overlay));
        r.reconcile(SplitLayout::Docked);
        assert!(r.is_open(SplitLayout::Docked), "back to docked default = open");
    }

    #[test]
    fn rail_snap_settles_without_animating() {
        let mut r = RailFlap::default();
        r.snap(false);
        assert!(!r.is_animating(SplitLayout::Overlay), "snap leaves nothing to animate");
        assert_eq!(r.reveal(), 0.0);
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
