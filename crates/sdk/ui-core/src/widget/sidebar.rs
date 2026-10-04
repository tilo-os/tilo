//! `SidebarDrawer` — the headless sidebar controller for painted apps.
//!
//! A widget app uses [`crate::widget::NavShell`]; an app that paints its
//! whole window itself holds a `SidebarDrawer` instead. The drawer owns the
//! same decisions the shell makes, from the same metrics: docked beside the
//! content when the window is wide, a floating drawer over it (behind a
//! scrim) when it is narrow, the eased reveal, and the person's show/hide
//! choice per size class. It hands back the resolved rects; the host paints
//! the panel with [`SidebarDrawer::paint_panel`], the scrim with
//! [`SidebarDrawer::draw_scrim`] and the toggle with
//! [`SidebarDrawer::draw_toggle`], and its rows with
//! [`crate::chrome::paint_nav_row`].
//!
//! Lifecycle per frame: [`SidebarDrawer::tick`] once from the draw path,
//! then [`SidebarDrawer::resolve`] with the outer rect. On the event path
//! call `resolve` alone so hit-tests match the last paint.

use crate::time::Instant;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::{Metrics, Spacing};
use crate::widget::AuthoredExtent;
use crate::widget::flap::{FlapState, SplitLayout};

/// The docked sidebar width every shell shares (`Metrics::sidebar_width`).
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const, and an f32 -> f64 widening is exact"
)]
pub const DEFAULT_DRAWER_DOCKED_WIDTH: f64 = Metrics::BASE.sidebar_width as f64;
/// The overlay drawer width (`Metrics::drawer_width`).
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const, and an f32 -> f64 widening is exact"
)]
pub const DEFAULT_DRAWER_WIDTH: f64 = Metrics::BASE.drawer_width as f64;
/// At or above this width the drawer docks (`Metrics::compact_breakpoint`).
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const, and an f32 -> f64 widening is exact"
)]
pub const DEFAULT_DRAWER_BREAKPOINT: f64 = Metrics::BASE.compact_breakpoint as f64;
/// Upper bound (seconds) on the per-frame delta used to advance the slide, so
/// a stale clock (first frame, or a parked event loop) can't teleport it.
const MAX_FRAME_DT: f64 = 0.1;
/// How far an open overlay drawer dims what is behind it.
pub(crate) const DRAWER_SCRIM_STRENGTH: f32 = 0.3;

/// Narrows an eased reveal to `f32` for vello.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a reveal is clamped to 0..=1, always well within f32 range"
)]
fn unit_f32(v: f64) -> f32 {
    v.clamp(0.0, 1.0) as f32
}

/// Which edge the drawer is anchored to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarSide {
    /// Anchored to the leading edge: a navigation sidebar.
    Left,
    /// Anchored to the trailing edge: an inspector (calculator history).
    Right,
}

/// A collapsible sidebar or inspector for a painted app. See the module
/// docs.
#[derive(Clone, Debug)]
pub struct SidebarDrawer {
    side: SidebarSide,
    /// Authored at 1× (HIG §9.6, [`AuthoredExtent`]); resolved through the
    /// factors [`Self::scale_for`] caches.
    breakpoint: AuthoredExtent,
    docked_width: AuthoredExtent,
    overlay_width: AuthoredExtent,
    /// Width left behind when collapsed while docked. `0.0` ⇒ fully hidden.
    /// An icon rail: follows `ui_scale`, never the text.
    rail_width: AuthoredExtent,
    /// `Theme::responsive_scale` and `Theme::ui_scale` of the theme last
    /// handed to [`Self::scale_for`]; `1.0` until then.
    responsive_scale: f64,
    ui_scale: f64,
    /// The `s2` gap an overlay drawer floats inside, from the same theme.
    float_inset: f64,
    flap: FlapState,

    // --- resolved geometry (last `resolve`) ---
    outer: Rect,
    layout: SplitLayout,
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
    /// A leading navigation drawer, open when docked, at the shell metrics.
    pub fn new() -> Self {
        Self {
            side: SidebarSide::Left,
            breakpoint: AuthoredExtent::text(DEFAULT_DRAWER_BREAKPOINT),
            docked_width: AuthoredExtent::text(DEFAULT_DRAWER_DOCKED_WIDTH),
            overlay_width: AuthoredExtent::text(DEFAULT_DRAWER_WIDTH),
            rail_width: AuthoredExtent::icon(0.0),
            responsive_scale: 1.0,
            ui_scale: 1.0,
            float_inset: f64::from(Spacing::BASE.s2),
            flap: FlapState::open_when_docked(),
            outer: Rect::ZERO,
            layout: SplitLayout::Docked,
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
    /// Authored at 1× (text-sized); see [`Self::scale_for`].
    #[must_use]
    pub fn breakpoint(mut self, px: f64) -> Self {
        self.breakpoint = AuthoredExtent::text(px);
        self
    }
    /// Docked width of a text-sized drawer (a page or destination list),
    /// authored at 1×.
    #[must_use]
    pub fn docked_width(mut self, px: f64) -> Self {
        self.docked_width = AuthoredExtent::text(px);
        self
    }
    /// Docked width of an **icon rail**: follows `ui_scale`, never the text.
    #[must_use]
    pub fn icon_docked_width(mut self, px: f64) -> Self {
        self.docked_width = AuthoredExtent::icon(px);
        self
    }
    /// Overlay width of the text-sized drawer, authored at 1×.
    #[must_use]
    pub fn overlay_width(mut self, px: f64) -> Self {
        self.overlay_width = AuthoredExtent::text(px);
        self
    }
    /// `0.0` hides it entirely. An icon rail: `ui_scale` only.
    #[must_use]
    pub fn rail_width(mut self, px: f64) -> Self {
        self.rail_width = AuthoredExtent::icon(px);
        self
    }
    /// A summon-on-demand panel.
    #[must_use]
    pub fn closed(mut self) -> Self {
        self.flap = FlapState::closed_by_default();
        self
    }

    // --- dynamic setters (per-frame reconfig) -------------------------------
    //
    // Each keeps the sizing its builder chose (a rail stays a rail).

    pub fn set_docked_width(&mut self, px: f64) {
        self.docked_width = self.docked_width.with_px(px);
    }
    pub fn set_overlay_width(&mut self, px: f64) {
        self.overlay_width = self.overlay_width.with_px(px);
    }
    pub fn set_breakpoint(&mut self, px: f64) {
        self.breakpoint = self.breakpoint.with_px(px);
    }

    /// Adopt the theme's scale so the authored breakpoint and the
    /// text-sized widths grow by [`Theme::responsive_scale`], an icon rail by
    /// `ui_scale` (HIG §9.6), and the overlay drawer floats inside the
    /// theme's `s2`. Call it from the host's `build_layout` with the theme it
    /// receives. Never called ⇒ authored geometry.
    pub fn scale_for(&mut self, theme: &Theme) {
        self.responsive_scale = theme.responsive_scale();
        self.ui_scale = f64::from(theme.ui_scale);
        self.float_inset = f64::from(theme.tokens.spacing.s2);
    }

    fn resolve_extent(&self, extent: AuthoredExtent) -> f64 {
        extent.resolve_with(self.responsive_scale, self.ui_scale)
    }

    /// The breakpoint as laid out (text-sized).
    pub fn resolved_breakpoint(&self) -> f64 {
        self.resolve_extent(self.breakpoint)
    }
    /// The docked width as laid out.
    pub fn resolved_docked_width(&self) -> f64 {
        self.resolve_extent(self.docked_width)
    }
    /// The overlay width as laid out (text-sized).
    pub fn resolved_overlay_width(&self) -> f64 {
        self.resolve_extent(self.overlay_width)
    }
    /// The collapsed rail width as laid out (icon-sized: `ui_scale` only).
    pub fn resolved_rail_width(&self) -> f64 {
        self.resolve_extent(self.rail_width)
    }

    // --- state --------------------------------------------------------------

    pub fn anchored_side(&self) -> SidebarSide {
        self.side
    }
    /// Docked at/above the (resolved) breakpoint.
    pub fn layout_for(&self, width: f64) -> SplitLayout {
        SplitLayout::from_width(width, self.resolved_breakpoint())
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
    /// Flip open/closed and remember the person's choice for this size
    /// class.
    pub fn toggle(&mut self) {
        self.flap.toggle(self.layout);
    }
    /// Force open (remembering the choice).
    pub fn open(&mut self) {
        self.flap.open();
    }
    /// Force closed (scrim / Esc / a choice made in the drawer).
    pub fn close(&mut self) {
        self.flap.close();
    }
    /// Snap to `open` with NO animation — deterministic `--shots` / tests.
    pub fn snap(&mut self, open: bool) {
        self.flap.snap(open);
    }
    /// The eased reveal in `0..=1` (scrim strength, docked inset).
    pub fn reveal(&self) -> f64 {
        self.flap.reveal()
    }
    /// `true` while the slide hasn't settled.
    pub fn is_animating(&self) -> bool {
        self.animating || self.flap.is_animating(self.layout)
    }

    // --- geometry -----------------------------------------------------------

    /// The sidebar panel. An overlay drawer floats `s2` inside the outer
    /// rect; its contents lay out inside this rect.
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

    /// Advance the eased reveal off a real wall-clock delta over the
    /// theme's `motion.durations.base`. Call once per draw. Under reduced
    /// motion the reveal jumps to its target. The first tick establishes
    /// the clock (`dt == 0`); later deltas are clamped to `MAX_FRAME_DT`.
    /// Returns whether it is still animating.
    pub fn tick(&mut self, theme: &Theme) -> bool {
        let now = crate::time::now();
        let dt = self.last_tick.map_or(0.0, |t| {
            now.duration_since(t).as_secs_f64().min(MAX_FRAME_DT)
        });
        self.last_tick = Some(now);

        if theme.reduce_motion {
            self.flap.settle(self.layout);
            self.animating = false;
        } else {
            self.animating = self
                .flap
                .tick(self.layout, dt, theme.tokens.motion.durations.base());
        }
        self.recompute_rects();
        self.animating
    }

    /// Resolve the docked-vs-overlay layout + all rects for `outer`. Cheap and
    /// deterministic (no clock); call from both the draw and event paths.
    /// Returns the content rect.
    pub fn resolve(&mut self, outer: Rect) -> Rect {
        self.outer = outer;
        self.layout = self.layout_for(outer.width());
        self.flap.reconcile(self.layout);
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
                let full = self.resolved_docked_width().min(o.width());
                let rail = self.resolved_rail_width().min(full);
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
                // Content spans the full width; the drawer floats in over it.
                self.content_rect = o;
                let gap = self.float_inset;
                let slide = self.flap.reveal();
                let w = self
                    .resolved_overlay_width()
                    .min((o.width() - 2.0 * gap).max(0.0));
                let travel = w + gap;
                let (y0, y1) = (o.y0 + gap, (o.y1 - gap).max(o.y0 + gap));
                self.sidebar_rect = match self.side {
                    SidebarSide::Left => {
                        let x0 = o.x0 + gap - travel * (1.0 - slide);
                        Rect::new(x0, y0, x0 + w, y1)
                    }
                    SidebarSide::Right => {
                        let x1 = o.x1 - gap + travel * (1.0 - slide);
                        Rect::new(x1 - w, y0, x1, y1)
                    }
                };
                self.scrim_rect = if self.flap.is_open(self.layout) || slide > 0.001 {
                    o
                } else {
                    Rect::ZERO
                };
            }
        }
    }

    // --- hit-testing --------------------------------------------------------

    /// Whether `p` is over the drawer panel.
    pub fn sidebar_contains(&self, p: Point) -> bool {
        self.sidebar_rect.width() > 0.0 && self.sidebar_rect.contains(p)
    }
    /// Whether `p` landed on the dismiss scrim of an open overlay drawer.
    pub fn scrim_contains(&self, p: Point) -> bool {
        self.is_overlay()
            && self.scrim_rect.width() > 0.0
            && self.scrim_rect.contains(p)
            && !self.sidebar_contains(p)
    }
    /// If `p` dismisses an open overlay drawer (a scrim tap), close it and
    /// report `true`.
    pub fn press_dismisses(&mut self, p: Point) -> bool {
        if self.is_overlay() && self.is_open() && self.scrim_contains(p) {
            self.close();
            true
        } else {
            false
        }
    }

    // --- drawing ------------------------------------------------------------

    /// Paint the scrim behind an open overlay drawer (`surfaces.scrim`,
    /// strength following the reveal). Call after the content is drawn and
    /// before [`Self::paint_panel`]. No-op when docked.
    pub fn draw_scrim(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if self.scrim_rect.width() <= 0.0 {
            return;
        }
        let strength = DRAWER_SCRIM_STRENGTH * unit_f32(self.flap.reveal());
        crate::chrome::fill_scrim(scene, transform, self.scrim_rect, strength, theme);
    }

    /// Paint the panel the drawer's contents stand on and return the theme
    /// to draw them with. Docked, it is the window region
    /// (`Region::Sidebar`, or `Region::Inspector` for a trailing drawer)
    /// with no radius and no shadow; as an overlay it is the floating
    /// material with the panel radius and the floating shadow.
    pub fn paint_panel(&self, scene: &mut Scene, transform: Affine, theme: &Theme) -> Theme {
        let rect = self.sidebar_rect;
        if self.is_overlay() {
            paint_floating_drawer(scene, transform, rect, theme)
        } else {
            let region = match self.side {
                SidebarSide::Left => crate::surface::Region::Sidebar,
                SidebarSide::Right => crate::surface::Region::Inspector,
            };
            crate::surface::region(scene, transform, rect, region, theme);
            theme.in_region(region)
        }
    }

    /// Paint the show/hide control inside `rect`: the same Plain icon
    /// button `NavShell` draws (`Icon::PanelLeft`, or `PanelRight` for a
    /// trailing drawer), transparent at rest and lifted under the pointer.
    /// Draw it with the theme of the ground it stands on.
    pub fn draw_toggle(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        hovered: bool,
    ) {
        let (icon, name) = match (self.side, self.is_open()) {
            (SidebarSide::Left, true) => (crate::icons::Icon::PanelLeft, "Hide sidebar"),
            (SidebarSide::Left, false) => (crate::icons::Icon::PanelLeft, "Show sidebar"),
            (SidebarSide::Right, true) => (crate::icons::Icon::PanelRight, "Hide inspector"),
            (SidebarSide::Right, false) => (crate::icons::Icon::PanelRight, "Show inspector"),
        };
        let face = crate::widget::ButtonFace::icon_only(rect, icon, name).hovered(hovered);
        crate::widget::Button::paint(scene, transform, text_renderer, theme, &face);
    }
}

/// The floating drawer surface: the floating shadow, the floating material
/// with its edge, at the panel radius. Returns the theme for what stands on
/// it.
pub(crate) fn paint_floating_drawer(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    theme: &Theme,
) -> Theme {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return theme.on_ground(crate::chrome::panel_ground(theme));
    }
    let radius = f64::from(theme.tokens.radii.panel);
    crate::surface::shadow(
        scene,
        transform,
        rect,
        radius,
        &theme.tokens.elevation.floating,
    );
    crate::surface::panel(scene, transform, rect, radius, theme);
    theme.on_ground(crate::chrome::panel_ground(theme))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HANG_BOUND;
    use vello::kurbo::Affine;

    fn wide() -> Rect {
        Rect::new(0.0, 0.0, 1200.0, 800.0)
    }
    fn narrow() -> Rect {
        Rect::new(0.0, 0.0, 600.0, 800.0)
    }

    #[test]
    fn drawer_shares_the_shell_metrics() {
        let mut d = SidebarDrawer::new();
        d.scale_for(&Theme::dark());
        let metrics = Metrics::BASE;
        assert_eq!(d.resolved_docked_width(), f64::from(metrics.sidebar_width));
        assert_eq!(d.resolved_overlay_width(), f64::from(metrics.drawer_width));
        assert_eq!(
            d.resolved_breakpoint(),
            f64::from(metrics.compact_breakpoint)
        );
    }

    #[test]
    fn sidebar_docks_wide_overlays_narrow_and_mirrors_sides() {
        // Case `wide_docks_and_insets_content_right_side`.
        {
            let mut d = SidebarDrawer::new().right().docked_width(300.0);
            let content = d.resolve(wide());
            assert!(d.is_docked(), "wide width docks");
            assert!(d.is_open(), "docked default is open");
            assert!((content.x0 - wide().x0).abs() < 0.5);
            assert!((wide().x1 - content.x1 - 300.0).abs() < 0.5);
            assert_eq!(d.scrim_rect(), Rect::ZERO, "docked draws no scrim");
            assert!((d.sidebar_rect().x1 - wide().x1).abs() < 0.5);
            assert!((d.sidebar_rect().width() - 300.0).abs() < 0.5);
        }

        // Case `narrow_overlays_closed_by_default_full_width_content`.
        {
            let mut d = SidebarDrawer::new().right().overlay_width(320.0);
            let content = d.resolve(narrow());
            assert!(d.is_overlay(), "narrow width overlays");
            assert!(!d.is_open(), "overlay default is closed");
            assert!((content.width() - narrow().width()).abs() < 0.5);
            assert_eq!(d.scrim_rect(), Rect::ZERO);
            assert!(
                d.sidebar_rect().x0 >= narrow().x1 - 0.5,
                "closed right drawer is off-screen to the right"
            );
        }

        // Case `left_side_mirrors_right`.
        {
            let mut d = SidebarDrawer::new().left().docked_width(280.0);
            let content = d.resolve(wide());
            assert!(d.is_docked() && d.is_open());
            assert!((content.x0 - wide().x0 - 280.0).abs() < 0.5);
            assert!((d.sidebar_rect().x0 - wide().x0).abs() < 0.5);
        }

        // Case `rail_leaves_a_thin_strip_when_collapsed_docked`.
        {
            let mut d = SidebarDrawer::new()
                .left()
                .docked_width(280.0)
                .rail_width(56.0)
                .closed();
            let content = d.resolve(wide());
            assert!((content.x0 - wide().x0 - 56.0).abs() < 0.5);
            d.open();
            d.snap(true);
            d.resolve(wide());
            assert!((d.content_rect().x0 - wide().x0 - 280.0).abs() < 0.5);
        }

        // Case `a_hidden_sidebar_stays_hidden_across_a_narrow_window`.
        {
            let mut d = SidebarDrawer::new().right();
            d.resolve(wide());
            d.close();
            d.resolve(wide());
            assert!(!d.is_open(), "explicit collapse holds while docked");
            d.resolve(narrow());
            d.resolve(wide());
            assert!(
                !d.is_open(),
                "the choice is remembered for the wide size class"
            );
        }
    }

    #[test]
    fn overlay_drawer_floats_slides_with_scrim_and_dismisses_on_scrim_only() {
        let theme = Theme::dark();
        let gap = f64::from(theme.tokens.spacing.s2);
        // Case `toggle_open_overlay_paints_scrim_and_floats_in`.
        {
            let mut d = SidebarDrawer::new().right().overlay_width(320.0);
            d.scale_for(&theme);
            d.resolve(narrow());
            d.toggle();
            d.snap(true);
            d.resolve(narrow());
            assert!(d.is_open());
            assert!(d.scrim_rect().width() > 0.0, "open overlay lays a scrim");
            assert!(
                (d.sidebar_rect().x1 - (narrow().x1 - gap)).abs() < 0.5,
                "an open drawer floats s2 inside the trailing edge"
            );
            assert!((d.sidebar_rect().y0 - gap).abs() < 0.5);
        }

        // Case `scrim_tap_dismisses_overlay_but_a_tap_on_the_drawer_does_not`.
        {
            let mut d = SidebarDrawer::new().right().overlay_width(320.0);
            d.resolve(narrow());
            d.open();
            d.snap(true);
            d.resolve(narrow());
            assert!(!d.press_dismisses(d.sidebar_rect().center()));
            assert!(d.is_open());
            let scrim_pt = Point::new(narrow().x0 + 20.0, 400.0);
            assert!(d.scrim_contains(scrim_pt));
            assert!(d.press_dismisses(scrim_pt));
            assert!(!d.is_open());
        }

        // Case `overlay_flap_slides_across_frames_from_the_tick_path`.
        {
            let mut d = SidebarDrawer::new().right().overlay_width(320.0);
            d.resolve(narrow());
            d.snap(false);
            assert_eq!(d.reveal(), 0.0);
            d.open();

            d.tick(&theme);
            assert_eq!(d.reveal(), 0.0, "first tick must not teleport the slide");

            std::thread::sleep(crate::time::Duration::from_millis(16));
            d.tick(&theme);
            let r = d.reveal();
            assert!(r > 0.0 && r < 1.0, "mid-slide after one frame ({r})");
            assert!(d.is_animating());

            let deadline = crate::time::now() + HANG_BOUND;
            while crate::time::now() < deadline {
                std::thread::sleep(crate::time::Duration::from_millis(1));
                d.tick(&theme);
                if !d.is_animating() {
                    break;
                }
            }
            assert!((d.reveal() - 1.0).abs() < 0.01);
        }

        // Case `reduce_motion_snaps_instead_of_sliding`.
        {
            let mut calm = Theme::dark();
            calm.reduce_motion = true;
            let mut d = SidebarDrawer::new().right().overlay_width(320.0);
            d.resolve(narrow());
            d.snap(false);
            d.open();
            d.tick(&calm);
            assert_eq!(d.reveal(), 1.0);
            assert!(!d.is_animating());
        }

        // Case `panel_toggle_and_scrim_paint_from_tokens`.
        {
            let mut d = SidebarDrawer::new().overlay_width(320.0);
            d.resolve(narrow());
            d.open();
            d.snap(true);
            d.resolve(narrow());
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            d.draw_scrim(&mut scene, Affine::IDENTITY, &theme);
            let drawer_theme = d.paint_panel(&mut scene, Affine::IDENTITY, &theme);
            assert_eq!(
                drawer_theme.tokens.colors.surfaces.content,
                crate::chrome::panel_ground(&theme),
                "drawer contents stand on the floating material"
            );
            d.draw_toggle(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &drawer_theme,
                Rect::new(12.0, 12.0, 44.0, 44.0),
                true,
            );
            assert!(scene.encoding().draw_tags.len() >= 3);

            let mut docked = SidebarDrawer::new();
            docked.resolve(wide());
            let docked_theme = docked.paint_panel(&mut Scene::new(), Affine::IDENTITY, &theme);
            assert_eq!(
                docked_theme.tokens.colors.surfaces.content,
                theme.region(crate::surface::Region::Sidebar)
            );
        }
    }

    #[test]
    fn scale_for_grows_text_sized_extents_and_leaves_the_icon_rail() {
        let authored = || {
            SidebarDrawer::new()
                .breakpoint(760.0)
                .docked_width(240.0)
                .overlay_width(320.0)
                .rail_width(56.0)
        };
        let mut plain = authored();
        plain.resolve(Rect::new(0.0, 0.0, 1000.0, 800.0));
        assert_eq!(plain.layout(), SplitLayout::Docked);
        assert_eq!(plain.sidebar_rect().width(), 240.0);
        assert_eq!(
            (
                plain.resolved_breakpoint(),
                plain.resolved_overlay_width(),
                plain.resolved_rail_width()
            ),
            (760.0, 320.0, 56.0)
        );

        let mut large = authored();
        large.scale_for(&Theme::dark().with_text_scale(2.0));
        assert_eq!(
            (
                large.resolved_breakpoint(),
                large.resolved_docked_width(),
                large.resolved_overlay_width()
            ),
            (1520.0, 480.0, 640.0)
        );
        assert_eq!(large.resolved_rail_width(), 56.0);
        large.resolve(Rect::new(0.0, 0.0, 1000.0, 800.0));
        assert_eq!(large.layout(), SplitLayout::Overlay);

        let mut smaller = authored();
        smaller.scale_for(&Theme::dark().with_text_scale(0.75));
        assert_eq!(smaller.resolved_docked_width(), 240.0);

        let mut dense = authored();
        dense.scale_for(&Theme::dark().with_ui_scale(1.5));
        assert!((dense.resolved_rail_width() - 84.0).abs() < 1e-9);
        assert!((dense.resolved_breakpoint() - 1140.0).abs() < 1e-9);

        let mut glyphs = SidebarDrawer::new()
            .icon_docked_width(56.0)
            .overlay_width(288.0);
        glyphs.scale_for(&Theme::dark().with_text_scale(2.0));
        assert_eq!(glyphs.resolved_docked_width(), 56.0);
        assert_eq!(glyphs.resolved_overlay_width(), 576.0);
        glyphs.set_docked_width(64.0);
        assert_eq!(glyphs.resolved_docked_width(), 64.0);
    }
}
