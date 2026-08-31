//! `SidebarDrawer` — a reusable collapsible sidebar / drawer primitive.
//!
//! Apps hand-rolled the same "dock-vs-overlay sidebar + scrim + collapse
//! toggle + `user_toggled`" pattern repeatedly (the calculator alone three
//! times), so this promotes it into one theme-driven, retained-mode
//! controller.
//!
//! Unlike [`crate::widget::NavSplit`] (a full [`Widget`](crate::widget::Widget)
//! with its own inner Taffy tree), `SidebarDrawer` is a *headless* geometry +
//! state controller: it owns the responsive docked-vs-overlay decision, the
//! eased reveal, the scrim, and the user-toggle memory, and hands back the
//! resolved rects — usable from both widget-tree hosts and the immediate-mode
//! Vello painters the calculator's panels are built from (which can't embed a
//! `Widget`).
//!
//! Behaviour (the Adwaita `OverlaySplitView` "flap" idiom): **docked** (wide)
//! sits beside the content, inset by the eased sidebar width, and collapsing
//! either hides it or leaves a rail ([`SidebarDrawer::rail_width`]);
//! **overlay** (narrow) draws OVER the content behind a scrim, closed by
//! default, dismissed by the scrim or a caller-routed Esc. The
//! docked-vs-overlay split is re-decided every frame from the available
//! width; an explicit user toggle is remembered until the layout crosses the
//! breakpoint, whereupon it reverts to the width-driven default.
//!
//! The eased reveal reuses the [`FlapState`] kernel from weather / chat /
//! `NavSplit`, so the slide feels identical across the OS.

use std::time::Instant;

use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use crate::color_compat::ColorCompat;
use crate::theme::Theme;
use crate::widget::nav_split::{FlapState, SplitLayout};

pub const DEFAULT_DRAWER_WIDTH: f64 = 320.0;
/// At/above this width the drawer docks; below it overlays.
pub const DEFAULT_DRAWER_BREAKPOINT: f64 = 760.0;
/// Upper bound (seconds) on the per-frame delta used to advance the slide, so
/// a stale clock (first frame, or a parked event loop) can't teleport it.
const MAX_FRAME_DT: f64 = 0.1;

/// Narrows a screen-space/eased-reveal `f64` value to `f32` for vello;
/// every call site here is a bounded `0.0..=1.0` fraction or px value.
#[allow(
    clippy::cast_possible_truncation,
    reason = "bounded 0..1 fraction or screen-space px value, always well within f32 range"
)]
fn px(v: f64) -> f32 {
    v as f32
}

/// Widens a small bounded UI loop count (shadow-band step index/count,
/// always single digits) to `f64` for interpolation math.
#[allow(
    clippy::cast_precision_loss,
    reason = "loop step counts here are always small, far below f64's exact-integer range"
)]
fn count_f64(n: usize) -> f64 {
    n as f64
}

/// As [`count_f64`], narrowing to `f32`.
#[allow(
    clippy::cast_precision_loss,
    reason = "loop step counts here are always small, far below f32's exact-integer range"
)]
fn count_f32(n: usize) -> f32 {
    n as f32
}

/// Which edge the drawer is anchored to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarSide {
    /// Anchored to the left edge; content sits to its right.
    Left,
    /// Anchored to the right edge; content sits to its left (calculator
    /// history).
    Right,
}

/// A reusable collapsible sidebar / drawer. See the module docs.
///
/// Lifecycle per frame: call [`SidebarDrawer::tick`] once from the draw path
/// (advances the eased reveal off a real wall-clock delta), then
/// [`SidebarDrawer::resolve`] with the outer rect to compute geometry. On the
/// event path call `resolve` alone (no tick) so hit-tests match the last paint.
#[derive(Clone, Debug)]
pub struct SidebarDrawer {
    side: SidebarSide,
    breakpoint: f64,
    docked_width: f64,
    overlay_width: f64,
    /// Width left behind when collapsed while docked. `0.0` ⇒ fully hidden.
    rail_width: f64,
    flap: FlapState,

    // --- resolved geometry (last `resolve`) ---
    outer: Rect,
    layout: SplitLayout,
    /// The layout the previous `resolve` produced, to detect the first frame
    /// and breakpoint crossings (where the eased reveal must be re-settled).
    resolved_once: Option<SplitLayout>,
    sidebar_rect: Rect,
    content_rect: Rect,
    scrim_rect: Rect,

    // --- self-tick animation ---
    last_tick: Option<Instant>,
    animating: bool,
}

impl Default for SidebarDrawer {
    fn default() -> Self {
        Self::new()
    }
}

impl SidebarDrawer {
    /// A left-anchored drawer, open when docked, with the default breakpoint
    /// and width.
    pub fn new() -> Self {
        Self {
            side: SidebarSide::Left,
            breakpoint: DEFAULT_DRAWER_BREAKPOINT,
            docked_width: DEFAULT_DRAWER_WIDTH,
            overlay_width: DEFAULT_DRAWER_WIDTH,
            rail_width: 0.0,
            flap: FlapState::open_when_docked(),
            outer: Rect::ZERO,
            layout: SplitLayout::Docked,
            resolved_once: None,
            sidebar_rect: Rect::ZERO,
            content_rect: Rect::ZERO,
            scrim_rect: Rect::ZERO,
            last_tick: None,
            animating: false,
        }
    }

    // --- builders -----------------------------------------------------------

    #[must_use]
    pub fn side(mut self, side: SidebarSide) -> Self {
        self.side = side;
        self
    }
    #[must_use]
    pub fn left(self) -> Self {
        self.side(SidebarSide::Left)
    }
    #[must_use]
    pub fn right(self) -> Self {
        self.side(SidebarSide::Right)
    }
    #[must_use]
    pub fn breakpoint(mut self, px: f64) -> Self {
        self.breakpoint = px;
        self
    }
    #[must_use]
    pub fn docked_width(mut self, px: f64) -> Self {
        self.docked_width = px;
        self
    }
    #[must_use]
    pub fn overlay_width(mut self, px: f64) -> Self {
        self.overlay_width = px;
        self
    }
    /// `0.0` hides it entirely.
    #[must_use]
    pub fn rail_width(mut self, px: f64) -> Self {
        self.rail_width = px;
        self
    }
    /// A summon-on-demand panel.
    #[must_use]
    pub fn closed(mut self) -> Self {
        self.flap = FlapState::closed_by_default();
        self
    }

    // --- dynamic setters (per-frame reconfig) -------------------------------

    /// E.g. a width computed as a fraction of the window each layout pass.
    pub fn set_docked_width(&mut self, px: f64) {
        self.docked_width = px;
    }
    pub fn set_overlay_width(&mut self, px: f64) {
        self.overlay_width = px;
    }
    pub fn set_breakpoint(&mut self, px: f64) {
        self.breakpoint = px;
    }

    // --- state --------------------------------------------------------------

    pub fn anchored_side(&self) -> SidebarSide {
        self.side
    }
    /// Docked at/above the breakpoint.
    pub fn layout_for(&self, width: f64) -> SplitLayout {
        SplitLayout::from_width(width, self.breakpoint)
    }
    pub fn layout(&self) -> SplitLayout {
        self.layout
    }
    pub fn is_overlay(&self) -> bool {
        self.layout == SplitLayout::Overlay
    }
    pub fn is_docked(&self) -> bool {
        self.layout == SplitLayout::Docked
    }
    pub fn is_open(&self) -> bool {
        self.flap.is_open(self.layout)
    }
    /// Flip open/closed and remember the user's explicit choice.
    pub fn toggle(&mut self) {
        self.flap.toggle(self.layout);
    }
    /// Force open (remembering the choice).
    pub fn open(&mut self) {
        self.flap.open();
    }
    /// Force closed (scrim / Esc / dismiss).
    pub fn close(&mut self) {
        self.flap.close();
    }
    /// Snap to `open` with NO animation — deterministic `--shots` / tests.
    pub fn snap(&mut self, open: bool) {
        self.flap.snap(open);
    }
    /// The eased reveal in `0..=1` (scrim alpha, docked inset).
    pub fn reveal(&self) -> f64 {
        self.flap.reveal()
    }
    /// `true` while the slide hasn't settled — drives the host's "keep pumping
    /// frames" decision.
    pub fn is_animating(&self) -> bool {
        self.animating || self.flap.is_animating(self.layout)
    }

    // --- geometry -----------------------------------------------------------

    pub fn sidebar_rect(&self) -> Rect {
        self.sidebar_rect
    }
    /// The usable area beside/under the drawer.
    pub fn content_rect(&self) -> Rect {
        self.content_rect
    }
    /// Overlay mode only; `Rect::ZERO` when absent.
    pub fn scrim_rect(&self) -> Rect {
        self.scrim_rect
    }

    /// Advance the eased reveal off a real wall-clock delta. Call once per
    /// draw. `reduce_motion` snaps straight to the target (accessibility). The
    /// first tick establishes the clock (`dt == 0`, no jump); later deltas are
    /// clamped to [`MAX_FRAME_DT`]. Returns whether it is still animating.
    pub fn tick(&mut self, reduce_motion: bool) -> bool {
        let now = Instant::now();
        let dt = self.last_tick.map_or(0.0, |t| {
            now.duration_since(t).as_secs_f64().min(MAX_FRAME_DT)
        });
        self.last_tick = Some(now);

        if reduce_motion {
            self.flap.snap(self.flap.is_open(self.layout));
            self.animating = false;
        } else {
            self.animating = self.flap.tick(self.layout, dt);
        }
        // Re-derive geometry from the freshly-eased reveal so the panes slide.
        self.recompute_rects();
        self.animating
    }

    /// Resolve the docked-vs-overlay layout + all rects for `outer`. Cheap and
    /// deterministic (no clock); call from both the draw and event paths.
    /// Returns the content rect (the usable area beside/under the drawer).
    pub fn resolve(&mut self, outer: Rect) -> Rect {
        self.outer = outer;
        self.layout = self.layout_for(outer.width());
        // Drop a stale user preference when the layout crosses the breakpoint.
        self.flap.reconcile(self.layout);
        // On the first resolve (or a docked↔overlay cross) snap the eased
        // reveal to the resolved default, so a flap that starts open_when_docked
        // doesn't paint a stale reveal (e.g. a scrim flash) at a width where the
        // default is closed. User toggles WITHIN a layout still animate.
        if self.resolved_once != Some(self.layout) {
            self.flap.settle(self.layout);
            self.resolved_once = Some(self.layout);
        }
        self.recompute_rects();
        self.content_rect
    }

    fn recompute_rects(&mut self) {
        let o = self.outer;
        if o.width() <= 0.0 {
            self.sidebar_rect = Rect::ZERO;
            self.content_rect = o;
            self.scrim_rect = Rect::ZERO;
            return;
        }
        match self.layout {
            SplitLayout::Docked => {
                let r = self.flap.reveal();
                let full = self.docked_width.min(o.width());
                let rail = self.rail_width.min(full);
                let inset = rail + (full - rail) * r;
                match self.side {
                    SidebarSide::Left => {
                        self.sidebar_rect = Rect::new(o.x0, o.y0, o.x0 + inset, o.y1);
                        self.content_rect = Rect::new(o.x0 + inset, o.y0, o.x1, o.y1);
                    }
                    SidebarSide::Right => {
                        self.sidebar_rect = Rect::new(o.x1 - inset, o.y0, o.x1, o.y1);
                        self.content_rect = Rect::new(o.x0, o.y0, o.x1 - inset, o.y1);
                    }
                }
                self.scrim_rect = Rect::ZERO;
            }
            SplitLayout::Overlay => {
                // Content spans the full width; the drawer slides in over it.
                self.content_rect = o;
                let slide = self.flap.slide();
                let w = self.overlay_width.min(o.width());
                self.sidebar_rect = match self.side {
                    SidebarSide::Left => {
                        let x0 = o.x0 - w * (1.0 - slide);
                        Rect::new(x0, o.y0, x0 + w, o.y1)
                    }
                    SidebarSide::Right => {
                        let x1 = o.x1 + w * (1.0 - slide);
                        Rect::new(x1 - w, o.y0, x1, o.y1)
                    }
                };
                // Present the scrim as soon as the target is open (so the very
                // first frame after a toggle already dims), and keep it while
                // the drawer slides back out.
                self.scrim_rect = if self.flap.is_open(self.layout) || self.flap.reveal() > 0.001 {
                    o
                } else {
                    Rect::ZERO
                };
            }
        }
    }

    // --- hit-testing --------------------------------------------------------

    /// Whether `p` is over the drawer panel (an overlay drawer captures clicks
    /// so they don't fall through to the content behind it).
    pub fn sidebar_contains(&self, p: Point) -> bool {
        self.sidebar_rect.width() > 0.0 && self.sidebar_rect.contains(p)
    }
    /// Whether `p` landed on the dismiss scrim of an open overlay drawer (i.e.
    /// outside the drawer while it's up).
    pub fn scrim_contains(&self, p: Point) -> bool {
        self.is_overlay()
            && self.scrim_rect.width() > 0.0
            && self.scrim_rect.contains(p)
            && !self.sidebar_contains(p)
    }
    /// If `p` dismisses an open overlay drawer (a scrim tap), close it and
    /// report `true`. A no-op (returns `false`) when docked or already closed.
    pub fn press_dismisses(&mut self, p: Point) -> bool {
        if self.is_overlay() && self.is_open() && self.scrim_contains(p) {
            self.close();
            true
        } else {
            false
        }
    }

    // --- drawing ------------------------------------------------------------

    /// Paint the dimming scrim behind an open overlay drawer. Alpha tracks the
    /// eased reveal so it fades in/out with the slide. No-op when docked.
    ///
    /// A modern overlay drawer *floats* over the content: a light scrim so the
    /// content recedes without being blacked out, plus a soft shadow at the
    /// drawer's trailing edge so it reads as elevated. A flat 50%-black fill over
    /// the whole UI looks heavy and dated — kept deliberately subtle here.
    pub fn draw_scrim(&self, scene: &mut Scene, transform: Affine) {
        if self.scrim_rect.width() <= 0.0 {
            return;
        }
        let reveal = px(self.flap.reveal().clamp(0.0, 1.0));
        if reveal <= 0.0 {
            return;
        }
        // Subtle dim (peaks at ~22%, not 50%).
        let scrim = Color::from_rgba8(0, 0, 0, 255).with_alpha_factor(reveal * 0.22);
        scene.fill(Fill::NonZero, transform, scrim, None, &self.scrim_rect);

        // Soft elevation shadow along the edge of the scrim that abuts the
        // drawer, so it reads as floating above the content rather than merely
        // dimming it. Stepped thin bands fake a gradient with decaying alpha —
        // `ui` has no gradient primitive, so this stays in plain fills.
        let sr = self.scrim_rect;
        let steps = 6usize;
        let band = 16.0_f64.min(sr.width());
        let w = band / count_f64(steps);
        for i in 0..steps {
            let t = count_f32(i) / count_f32(steps);
            let a = reveal * 0.15 * (1.0 - t);
            if a <= 0.001 {
                continue;
            }
            let c = Color::from_rgba8(0, 0, 0, 255).with_alpha_factor(a);
            let (bx0, bx1) = match self.side {
                SidebarSide::Left => (sr.x0 + w * count_f64(i), sr.x0 + w * (count_f64(i) + 1.0)),
                SidebarSide::Right => (sr.x1 - w * (count_f64(i) + 1.0), sr.x1 - w * count_f64(i)),
            };
            scene.fill(
                Fill::NonZero,
                transform,
                c,
                None,
                &Rect::new(bx0, sr.y0, bx1, sr.y1),
            );
        }
    }

    /// Paint the standard collapse affordance inside `rect`: a pill holding a
    /// side-panel glyph, tinted with the accent when the drawer is open. Purely
    /// theme-driven so the affordance reads the same across every app.
    pub fn draw_toggle(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
        hovered: bool,
    ) {
        let c = &theme.tokens.colors;
        let open = self.is_open();
        let radius = f64::from(theme.tokens.radii.md).min(rect.height() * 0.5);

        let bg = if open {
            mix(c.surfaces.raised, c.accent[ACCENT_IDX], 0.20)
        } else if hovered {
            c.neutral[3]
        } else {
            c.neutral[2]
        };
        let pill = RoundedRect::from_rect(rect, radius);
        scene.fill(Fill::NonZero, transform, bg, None, &pill);
        let border = if open {
            c.accent[ACCENT_IDX]
        } else {
            c.border_standard
        };
        scene.stroke(&Stroke::new(1.0), transform, border, None, &pill);

        // Panel glyph, centred: a rounded "window" with one edge filled to show
        // which side the drawer lives on.
        let g = glyph_box(rect);
        let gr = RoundedRect::from_rect(g, 2.5);
        let outline = if open {
            c.accent[ACCENT_IDX]
        } else {
            c.text_dim
        };
        scene.stroke(&Stroke::new(1.4), transform, outline, None, &gr);
        let bar_w = g.width() * 0.38;
        let bar = match self.side {
            SidebarSide::Left => Rect::new(g.x0, g.y0, g.x0 + bar_w, g.y1),
            SidebarSide::Right => Rect::new(g.x1 - bar_w, g.y0, g.x1, g.y1),
        };
        let bar_col = if open {
            c.accent[ACCENT_IDX]
        } else {
            // A faint, closed rail hint.
            with_alpha(c.text_dim, 90)
        };
        scene.fill(Fill::NonZero, transform, bar_col, None, &bar);
        // Divider between bar and body so the glyph reads at small sizes.
        let dx = match self.side {
            SidebarSide::Left => g.x0 + bar_w,
            SidebarSide::Right => g.x1 - bar_w,
        };
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            outline,
            None,
            &Line::new(Point::new(dx, g.y0), Point::new(dx, g.y1)),
        );
    }
}

/// Accent-ramp index used for active/open tinting (indigo-500).
const ACCENT_IDX: usize = 5;

/// The centred square glyph box inside a toggle pill.
fn glyph_box(rect: Rect) -> Rect {
    let side = (rect.height().min(rect.width()) - 12.0).clamp(12.0, 18.0);
    let cx = rect.center().x;
    let cy = rect.center().y;
    // Slightly wider than tall so it reads as a landscape "window".
    let w = side * 1.15;
    let h = side * 0.82;
    Rect::new(cx - w * 0.5, cy - h * 0.5, cx + w * 0.5, cy + h * 0.5)
}

fn mix(a: Color, b: Color, t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| {
        let blended = (f64::from(x) + (f64::from(y) - f64::from(x)) * t)
            .round()
            .clamp(0.0, 255.0);
        // blended is a convex combination of two u8-derived values (t
        // clamped to [0, 1] above), then clamped again immediately above,
        // so it can't wrap or flip sign on the way into u8.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "blended is clamped to [0.0, 255.0] on the line above"
        )]
        {
            blended as u8
        }
    };
    Color::from_rgba8(
        lerp(a.r(), b.r()),
        lerp(a.g(), b.g()),
        lerp(a.b(), b.b()),
        255,
    )
}

fn with_alpha(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Affine;

    fn wide() -> Rect {
        Rect::new(0.0, 0.0, 1200.0, 800.0)
    }
    fn narrow() -> Rect {
        Rect::new(0.0, 0.0, 600.0, 800.0)
    }

    #[test]
    fn wide_docks_and_insets_content_right_side() {
        let mut d = SidebarDrawer::new().right().docked_width(300.0);
        let content = d.resolve(wide());
        assert!(d.is_docked(), "wide width docks");
        assert!(d.is_open(), "docked default is open");
        assert!(
            (content.x0 - wide().x0).abs() < 0.5,
            "content keeps the left edge"
        );
        assert!(
            (wide().x1 - content.x1 - 300.0).abs() < 0.5,
            "content right edge inset by the docked sidebar width"
        );
        assert_eq!(d.scrim_rect(), Rect::ZERO, "docked draws no scrim");
        assert!((d.sidebar_rect().x1 - wide().x1).abs() < 0.5);
        assert!((d.sidebar_rect().width() - 300.0).abs() < 0.5);
    }

    #[test]
    fn narrow_overlays_closed_by_default_full_width_content() {
        let mut d = SidebarDrawer::new().right().overlay_width(320.0);
        let content = d.resolve(narrow());
        assert!(d.is_overlay(), "narrow width overlays");
        assert!(!d.is_open(), "overlay default is closed");
        assert!(
            (content.width() - narrow().width()).abs() < 0.5,
            "closed overlay leaves the content full-width"
        );
        assert_eq!(d.scrim_rect(), Rect::ZERO, "closed overlay draws no scrim");
        assert!(
            d.sidebar_rect().x0 >= narrow().x1 - 0.5,
            "closed right drawer is off-screen to the right"
        );
    }

    #[test]
    fn toggle_open_overlay_paints_scrim_and_slides_in() {
        let mut d = SidebarDrawer::new().right().overlay_width(320.0);
        d.resolve(narrow());
        d.toggle();
        d.snap(true); // settle deterministically
        d.resolve(narrow());
        assert!(d.is_open());
        assert!(d.scrim_rect().width() > 0.0, "open overlay lays a scrim");
        assert!(
            (d.sidebar_rect().x1 - narrow().x1).abs() < 0.5,
            "revealed right drawer is flush to the right edge"
        );
    }

    #[test]
    fn scrim_tap_dismisses_overlay_but_a_tap_on_the_drawer_does_not() {
        let mut d = SidebarDrawer::new().right().overlay_width(320.0);
        d.resolve(narrow());
        d.open();
        d.snap(true);
        d.resolve(narrow());
        // A tap on the drawer panel must NOT dismiss.
        assert!(!d.press_dismisses(d.sidebar_rect().center()));
        assert!(d.is_open());
        // Left of the drawer, in the scrim region.
        let scrim_pt = Point::new(narrow().x0 + 20.0, 400.0);
        assert!(d.scrim_contains(scrim_pt));
        assert!(d.press_dismisses(scrim_pt));
        assert!(!d.is_open());
    }

    #[test]
    fn left_side_mirrors_right() {
        let mut d = SidebarDrawer::new().left().docked_width(280.0);
        let content = d.resolve(wide());
        assert!(d.is_docked() && d.is_open());
        assert!(
            (content.x0 - wide().x0 - 280.0).abs() < 0.5,
            "left drawer insets content from the LEFT"
        );
        assert!((d.sidebar_rect().x0 - wide().x0).abs() < 0.5);
    }

    #[test]
    fn rail_leaves_a_thin_strip_when_collapsed_docked() {
        let mut d = SidebarDrawer::new()
            .left()
            .docked_width(280.0)
            .rail_width(56.0)
            .closed();
        let content = d.resolve(wide());
        assert!(
            (content.x0 - wide().x0 - 56.0).abs() < 0.5,
            "collapsed rail insets the content by the rail width, not the full drawer"
        );
        d.open();
        d.snap(true);
        d.resolve(wide());
        let content = d.content_rect();
        assert!(
            (content.x0 - wide().x0 - 280.0).abs() < 0.5,
            "opening a railed drawer reveals the full docked width"
        );
    }

    #[test]
    fn breakpoint_crossing_reverts_a_stale_toggle_to_the_width_default() {
        let mut d = SidebarDrawer::new().right();
        d.resolve(wide()); // docked, open by default
        d.close(); // user collapses it while docked
        d.resolve(wide());
        assert!(!d.is_open(), "explicit collapse holds while docked");
        // Shrink below the breakpoint, then grow back.
        d.resolve(narrow());
        d.resolve(wide());
        assert!(
            d.is_open(),
            "crossing the breakpoint reverts to the width-driven default (docked open)"
        );
    }

    #[test]
    fn overlay_flap_slides_across_frames_from_the_tick_path() {
        // The reveal must SLIDE across successive ticks, not snap.
        let mut d = SidebarDrawer::new().right().overlay_width(320.0);
        d.resolve(narrow());
        d.snap(false); // deterministic closed start
        assert_eq!(d.reveal(), 0.0);
        d.open();

        d.tick(false);
        assert_eq!(d.reveal(), 0.0, "first tick must not teleport the slide");

        std::thread::sleep(std::time::Duration::from_millis(16));
        d.tick(false);
        let r = d.reveal();
        assert!(
            r > 0.0 && r < 1.0,
            "after one ~16ms frame the flap is mid-slide (reveal {r})"
        );
        assert!(
            d.is_animating(),
            "a mid-slide drawer keeps requesting frames"
        );

        for _ in 0..300 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            d.tick(false);
            if !d.is_animating() {
                break;
            }
        }
        assert!(
            (d.reveal() - 1.0).abs() < 0.01,
            "the slide settles fully open (reveal {})",
            d.reveal()
        );
    }

    #[test]
    fn reduce_motion_snaps_instead_of_sliding() {
        let mut d = SidebarDrawer::new().right().overlay_width(320.0);
        d.resolve(narrow());
        d.snap(false);
        d.open();
        d.tick(true);
        assert_eq!(
            d.reveal(),
            1.0,
            "reduced motion snaps the reveal to its target"
        );
        assert!(!d.is_animating());
    }

    #[test]
    fn draw_toggle_and_scrim_emit_into_the_scene() {
        let mut d = SidebarDrawer::new().right().overlay_width(320.0);
        d.resolve(narrow());
        d.open();
        d.snap(true);
        d.resolve(narrow());
        let theme = Theme::dark();
        let mut scene = Scene::new();
        d.draw_scrim(&mut scene, Affine::IDENTITY);
        d.draw_toggle(
            &mut scene,
            Affine::IDENTITY,
            &theme,
            Rect::new(10.0, 10.0, 54.0, 38.0),
            false,
        );
        assert!(
            scene.encoding().draw_tags.len() >= 3,
            "toggle + scrim must paint"
        );
    }
}
