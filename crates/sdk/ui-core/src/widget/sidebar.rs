//! `SidebarDrawer` — a reusable collapsible sidebar / drawer primitive.
//!
//! Unlike [`crate::widget::NavSplit`] (a full [`Widget`](crate::widget::Widget)
//! with its own inner Taffy tree), `SidebarDrawer` is a *headless* geometry +
//! state controller: it owns the responsive docked-vs-overlay decision, the
//! eased reveal, the scrim, and the user-toggle memory, and hands back the
//! resolved rects for widget-tree hosts and immediate-mode Vello painters.
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
//! The eased reveal uses the shared [`FlapState`] kernel.

use crate::time::Instant;

use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};

use crate::color_compat::ColorCompat;
use crate::paint::with_alpha;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::Metrics;
use crate::widget::AuthoredExtent;
use crate::widget::nav_split::{FlapState, SplitLayout};

/// The OS-wide drawer / utility-panel width (`Metrics::drawer_width`).
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const, and an f32 -> f64 widening is exact"
)]
pub const DEFAULT_DRAWER_WIDTH: f64 = Metrics::BASE.drawer_width as f64;
/// At/above this width the drawer docks; below it overlays. 40 px over
/// `Metrics::compact_breakpoint` (720) because the drawer is 40 px wider
/// than the sidebar that breakpoint was sized for.
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

/// One destination in a [`SidebarNav`]. IDs are stable interaction targets;
/// labels and badges are presentation and may be localized or updated every
/// frame without changing routing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarNavItem {
    pub id: String,
    pub label: String,
    pub badge: Option<String>,
    pub selected: bool,
}

impl SidebarNavItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            badge: None,
            selected: false,
        }
    }

    #[must_use]
    pub fn badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

/// A labelled group of destinations in a [`SidebarNav`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarNavSection {
    pub label: String,
    pub items: Vec<SidebarNavItem>,
}

impl SidebarNavSection {
    pub fn new(label: impl Into<String>, items: Vec<SidebarNavItem>) -> Self {
        Self {
            label: label.into(),
            items,
        }
    }
}

/// Shared rendered navigation content for [`SidebarDrawer`].
///
/// Besides painting consistent section headings, selected rows, badges and
/// panel chrome, this owns the exact same row geometry used for hit testing.
/// Apps therefore never duplicate magic y-ranges that drift away from what is
/// visible. Every row is at least 44px high for touch and switch access.
#[derive(Clone, Debug)]
pub struct SidebarNav {
    rect: Rect,
    sections: Vec<SidebarNavSection>,
    row_height: f64,
    scroll_offset: f64,
}

impl SidebarNav {
    pub fn new(rect: Rect, sections: Vec<SidebarNavSection>) -> Self {
        Self {
            rect,
            sections,
            row_height: 46.0,
            scroll_offset: 0.0,
        }
    }

    /// Apply a caller-retained vertical scroll position. The value is clamped
    /// to the actual content range, so resize and dynamic section changes
    /// cannot leave the navigation stranded beyond its final row.
    #[must_use]
    pub fn with_scroll_offset(mut self, offset: f64) -> Self {
        self.scroll_offset = offset.clamp(0.0, self.max_scroll_offset());
        self
    }

    /// Total laid-out height of all headings and rows.
    pub fn content_height(&self) -> f64 {
        self.sections
            .iter()
            .map(|section| 54.0 + self.row_height * count_f64(section.items.len()))
            .sum()
    }

    /// Largest meaningful scroll position for this viewport.
    pub fn max_scroll_offset(&self) -> f64 {
        (self.content_height() - self.rect.height()).max(0.0)
    }

    /// The effective, range-clamped position used for paint and hit testing.
    pub fn scroll_offset(&self) -> f64 {
        self.scroll_offset.min(self.max_scroll_offset())
    }

    /// Build the semantic navigation tree from the same sections and selected
    /// state that are painted on screen. Immediate-mode hosts can therefore
    /// expose a complete sidebar without rebuilding a parallel, drift-prone
    /// list of accessibility-only buttons.
    pub fn accessibility(
        &self,
        name: impl Into<String>,
        action_target: impl Fn(&str) -> String,
    ) -> crate::a11y::AccessNode {
        use crate::a11y::{AccessNode, AccessState, Role};

        let children = self
            .sections
            .iter()
            .flat_map(|section| {
                section.items.iter().map(|item| {
                    let description = item.badge.as_ref().map_or_else(
                        || section.label.clone(),
                        |badge| format!("{}, {badge}", section.label),
                    );
                    AccessNode::new(Role::ListItem)
                        .with_name(&item.label)
                        .with_description(description)
                        .with_action_target(action_target(&item.id))
                        .with_state(AccessState::new().focusable(true).selected(item.selected))
                        .with_hashed_key(("sidebar-nav", &item.id))
                })
            })
            .collect();

        AccessNode::new(Role::List)
            .with_name(name.into())
            .with_children(children)
    }

    /// Stable destination under a mouse, pen, or touch point.
    pub fn hit_test(&self, point: Point) -> Option<&str> {
        // A short mobile viewport may show only part of a long navigation
        // list. Never let an off-screen row remain an invisible target below
        // the drawer (or through content beside it).
        if !self.rect.contains(point) {
            return None;
        }
        for (section_index, section) in self.sections.iter().enumerate() {
            for (item_index, item) in section.items.iter().enumerate() {
                let row = self.row_rect(section_index, item_index);
                if row.y0 < self.rect.y1 && row.y1 > self.rect.y0 && row.contains(point) {
                    return Some(item.id.as_str());
                }
            }
        }
        None
    }

    fn row_rect(&self, section_index: usize, item_index: usize) -> Rect {
        let prior_height: f64 = self
            .sections
            .iter()
            .take(section_index)
            .map(|section| 54.0 + self.row_height * count_f64(section.items.len()))
            .sum();
        let y = self.rect.y0 + 54.0 + prior_height + self.row_height * count_f64(item_index)
            - self.scroll_offset();
        Rect::new(
            self.rect.x0 + 8.0,
            y,
            self.rect.x1 - 8.0,
            y + self.row_height,
        )
    }

    pub fn render(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let panel = RoundedRect::from_rect(self.rect, 18.0);
        scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &panel);

        // Clip section content to the same rounded panel that is visible.
        // This matters on phones in landscape and with large text, where the
        // navigation can be taller than the available drawer.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &panel);

        let mut heading_y = self.rect.y0 + 20.0 - self.scroll_offset();
        for (section_index, section) in self.sections.iter().enumerate() {
            draw_nav_text(
                tr,
                scene,
                &section.label,
                (11.0, c.text_dim, TextWeight::Bold),
                transform,
                self.rect.x0 + 18.0,
                heading_y,
            );
            for (item_index, item) in section.items.iter().enumerate() {
                let row = self.row_rect(section_index, item_index);
                if item.selected {
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        c.surfaces.base,
                        None,
                        &RoundedRect::from_rect(row, 10.0),
                    );
                }
                draw_nav_text(
                    tr,
                    scene,
                    &item.label,
                    (
                        14.0,
                        c.text_primary,
                        if item.selected {
                            TextWeight::Bold
                        } else {
                            TextWeight::Medium
                        },
                    ),
                    transform,
                    row.x0 + 12.0,
                    row.y0 + 12.0,
                );
                if let Some(badge) = &item.badge {
                    let (width, _) = tr.measure_weighted(badge, 12.0, None, TextWeight::Regular);
                    draw_nav_text(
                        tr,
                        scene,
                        badge,
                        (12.0, c.text_dim, TextWeight::Regular),
                        transform,
                        row.x1 - f64::from(width) - 12.0,
                        row.y0 + 13.0,
                    );
                }
            }
            heading_y += 54.0 + self.row_height * count_f64(section.items.len());
        }
        scene.pop_layer();
    }
}

#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is bounded floating-point screen geometry"
)]
fn draw_nav_text(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    text: &str,
    style: (f32, Color, TextWeight),
    transform: Affine,
    x: f64,
    y: f64,
) {
    let (size, color, weight) = style;
    tr.draw_text_weighted(
        scene,
        text,
        size,
        color,
        transform * Affine::translate((x, y)),
        None,
        weight,
    );
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
    /// Authored at 1× (HIG §9.6, [`AuthoredExtent`]); resolved through the
    /// factors [`Self::scale_for`] caches.
    breakpoint: AuthoredExtent,
    docked_width: AuthoredExtent,
    overlay_width: AuthoredExtent,
    /// Width left behind when collapsed while docked. `0.0` ⇒ fully hidden.
    /// An icon rail: follows `ui_scale`, never the text.
    rail_width: AuthoredExtent,
    /// `Theme::responsive_scale` and `Theme::ui_scale` of the theme last
    /// handed to [`Self::scale_for`]; `1.0` until then, so a host that never
    /// calls it lays out exactly what it authored.
    responsive_scale: f64,
    ui_scale: f64,
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
            breakpoint: AuthoredExtent::text(DEFAULT_DRAWER_BREAKPOINT),
            docked_width: AuthoredExtent::text(DEFAULT_DRAWER_WIDTH),
            overlay_width: AuthoredExtent::text(DEFAULT_DRAWER_WIDTH),
            rail_width: AuthoredExtent::icon(0.0),
            responsive_scale: 1.0,
            ui_scale: 1.0,
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
    /// Docked width of an **icon rail** (firewall / backups / `api_tester`
    /// dock a glyph column): follows `ui_scale`, never the text.
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

    /// E.g. a width computed as a fraction of the window each layout pass.
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
    /// text-sized docked / overlay widths grow by [`Theme::responsive_scale`]
    /// and an icon rail by `ui_scale` (HIG §9.6) — the same rule the widget
    /// shells apply in `build_layout`. The drawer is a geometry helper with
    /// no `build_layout` of its own, so a host adopting the rule calls this
    /// from its `build_layout` with the theme it receives, once, and deletes
    /// its own `× text_scale` arithmetic. Never called ⇒ authored geometry,
    /// which is what every current host lays out.
    pub fn scale_for(&mut self, theme: &Theme) {
        self.responsive_scale = theme.responsive_scale();
        self.ui_scale = f64::from(theme.ui_scale);
    }

    fn resolve_extent(&self, extent: AuthoredExtent) -> f64 {
        extent.resolve_with(self.responsive_scale, self.ui_scale)
    }

    /// The breakpoint as laid out (text-sized).
    pub fn resolved_breakpoint(&self) -> f64 {
        self.resolve_extent(self.breakpoint)
    }
    /// The docked width as laid out (text-sized, or `ui_scale` only for an
    /// [`Self::icon_docked_width`]).
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
    /// clamped to `MAX_FRAME_DT`. Returns whether it is still animating.
    pub fn tick(&mut self, reduce_motion: bool) -> bool {
        let now = crate::time::now();
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
                // Content spans the full width; the drawer slides in over it.
                self.content_rect = o;
                let slide = self.flap.slide();
                let w = self.resolved_overlay_width().min(o.width());
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
        // dimming it. Stepped thin bands approximate a gradient with decaying alpha —
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

        // Rest face and edge follow the Neutral button (HIG §9.5): the
        // raised surface with its derived edge, `bg_hover` under the
        // pointer. Open is the toggle's "on" state and keeps the accent.
        let bg = if open {
            mix(c.surfaces.raised, c.accent[ACCENT_IDX], 0.20)
        } else if hovered {
            theme.bg_hover
        } else {
            c.surfaces.raised
        };
        let pill = RoundedRect::from_rect(rect, radius);
        scene.fill(Fill::NonZero, transform, bg, None, &pill);
        let border = if open {
            c.accent[ACCENT_IDX]
        } else {
            crate::surface::edge_on(bg)
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
    fn sidebar_docks_wide_overlays_narrow_and_mirrors_sides() {
        // Case `wide_docks_and_insets_content_right_side`.
        {
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

        // Case `narrow_overlays_closed_by_default_full_width_content`.
        {
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

        // Case `left_side_mirrors_right`.
        {
            let mut d = SidebarDrawer::new().left().docked_width(280.0);
            let content = d.resolve(wide());
            assert!(d.is_docked() && d.is_open());
            assert!(
                (content.x0 - wide().x0 - 280.0).abs() < 0.5,
                "left drawer insets content from the LEFT"
            );
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

        // Case `breakpoint_crossing_reverts_a_stale_toggle_to_the_width_default`.
        {
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
    }

    #[test]
    fn overlay_drawer_slides_with_scrim_and_dismisses_on_scrim_only() {
        // Case `toggle_open_overlay_paints_scrim_and_slides_in`.
        {
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

        // Case `scrim_tap_dismisses_overlay_but_a_tap_on_the_drawer_does_not`.
        {
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

        // Case `overlay_flap_slides_across_frames_from_the_tick_path`.
        {
            // The reveal must SLIDE across successive ticks, not snap.
            let mut d = SidebarDrawer::new().right().overlay_width(320.0);
            d.resolve(narrow());
            d.snap(false); // deterministic closed start
            assert_eq!(d.reveal(), 0.0);
            d.open();

            d.tick(false);
            assert_eq!(d.reveal(), 0.0, "first tick must not teleport the slide");

            std::thread::sleep(crate::time::Duration::from_millis(16));
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

            let deadline = crate::time::now() + HANG_BOUND;
            while crate::time::now() < deadline {
                std::thread::sleep(crate::time::Duration::from_millis(1));
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

        // Case `reduce_motion_snaps_instead_of_sliding`.
        {
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

        // Case `draw_toggle_and_scrim_emit_into_the_scene`.
        {
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

        // Case `scale_for_grows_text_sized_extents_and_leaves_the_icon_rail`.
        // HIG §9.6: once the host hands the drawer its theme, the authored
        // breakpoint and text-sized widths grow by `ui_scale × text_growth`
        // and the icon rail by `ui_scale` alone; a host that never calls
        // `scale_for` lays out exactly what it authored.
        {
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
            assert_eq!(
                plain.sidebar_rect().width(),
                240.0,
                "authored geometry until scaled"
            );
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
            assert_eq!(
                large.resolved_rail_width(),
                56.0,
                "an icon rail never grows with the text"
            );
            large.resolve(Rect::new(0.0, 0.0, 1000.0, 800.0));
            assert_eq!(
                large.layout(),
                SplitLayout::Overlay,
                "a 1000 px window at 2× has the room a 500 px window has at 1×"
            );
            large.open();
            large.snap(true);
            large.resolve(Rect::new(0.0, 0.0, 1000.0, 800.0));
            assert_eq!(
                large.sidebar_rect().width(),
                640.0,
                "the overlay drawer lays out at the resolved width"
            );

            let mut smaller = authored();
            smaller.scale_for(&Theme::dark().with_text_scale(0.75));
            assert_eq!(
                smaller.resolved_docked_width(),
                240.0,
                "text never shrinks a pane"
            );

            let mut dense = authored();
            dense.scale_for(&Theme::dark().with_ui_scale(1.5));
            assert!(
                (dense.resolved_rail_width() - 84.0).abs() < 1e-9,
                "ui_scale grows the rail too"
            );
            assert!((dense.resolved_breakpoint() - 1140.0).abs() < 1e-9);

            // A docked icon rail (firewall / backups) keeps its width at 2× text,
            // and a per-frame setter keeps the sizing its builder chose.
            let mut glyphs = SidebarDrawer::new()
                .icon_docked_width(56.0)
                .overlay_width(288.0);
            glyphs.scale_for(&Theme::dark().with_text_scale(2.0));
            assert_eq!(glyphs.resolved_docked_width(), 56.0);
            assert_eq!(glyphs.resolved_overlay_width(), 576.0);
            glyphs.set_docked_width(64.0);
            assert_eq!(
                glyphs.resolved_docked_width(),
                64.0,
                "still an icon rail after a setter"
            );
        }
    }

    #[test]
    fn navigation_rows_hit_test_scroll_and_describe_the_painted_model() {
        // Case `navigation_uses_painted_rows_for_mouse_pen_and_touch_hit_testing`.
        {
            let nav = SidebarNav::new(
                Rect::new(20.0, 10.0, 260.0, 700.0),
                vec![
                    SidebarNavSection::new(
                        "LIBRARY",
                        vec![
                            SidebarNavItem::new("all", "All items").selected(true),
                            SidebarNavItem::new("favourites", "Favourites"),
                        ],
                    ),
                    SidebarNavSection::new("TOOLS", vec![SidebarNavItem::new("import", "Import")]),
                ],
            );
            let all = nav.row_rect(0, 0);
            let favourites = nav.row_rect(0, 1);
            let import = nav.row_rect(1, 0);
            assert!(all.height() >= 44.0);
            assert_eq!(nav.hit_test(all.center()), Some("all"));
            assert_eq!(nav.hit_test(favourites.center()), Some("favourites"));
            assert_eq!(nav.hit_test(import.center()), Some("import"));
            assert_eq!(nav.hit_test(Point::new(21.0, 11.0)), None);
        }

        // Case `navigation_never_hits_rows_below_a_short_mobile_drawer`.
        {
            let nav = SidebarNav::new(
                Rect::new(0.0, 0.0, 280.0, 118.0),
                vec![SidebarNavSection::new(
                    "VAULT",
                    vec![
                        SidebarNavItem::new("visible", "Visible"),
                        SidebarNavItem::new("clipped", "Clipped"),
                        SidebarNavItem::new("offscreen", "Off screen"),
                    ],
                )],
            );

            assert_eq!(nav.hit_test(Point::new(40.0, 76.0)), Some("visible"));
            assert_eq!(nav.hit_test(Point::new(40.0, 122.0)), None);
            assert_eq!(nav.hit_test(Point::new(40.0, 168.0)), None);
        }

        // Case `navigation_scroll_reaches_clipped_mobile_rows_and_clamps_after_resize`.
        {
            let sections = vec![SidebarNavSection::new(
                "VAULT",
                vec![
                    SidebarNavItem::new("one", "One"),
                    SidebarNavItem::new("two", "Two"),
                    SidebarNavItem::new("three", "Three"),
                    SidebarNavItem::new("four", "Four"),
                ],
            )];
            let nav = SidebarNav::new(Rect::new(0.0, 0.0, 280.0, 118.0), sections.clone())
                .with_scroll_offset(f64::MAX);
            assert_eq!(nav.scroll_offset(), nav.max_scroll_offset());
            assert_eq!(nav.hit_test(Point::new(40.0, 96.0)), Some("four"));

            let resized = SidebarNav::new(Rect::new(0.0, 0.0, 280.0, 400.0), sections)
                .with_scroll_offset(nav.scroll_offset());
            assert_eq!(resized.scroll_offset(), 0.0);
        }

        // Case `navigation_semantics_share_labels_selection_and_actions_with_paint_model`.
        {
            use crate::a11y::Role;

            let nav = SidebarNav::new(
                Rect::new(0.0, 0.0, 280.0, 300.0),
                vec![SidebarNavSection::new(
                    "VAULT",
                    vec![
                        SidebarNavItem::new("all", "All items")
                            .badge("12")
                            .selected(true),
                        SidebarNavItem::new("favorites", "Favorites").badge("3"),
                    ],
                )],
            );
            let node = nav.accessibility("Vault navigation", |id| format!("filter:{id}"));

            assert_eq!(node.role, Role::List);
            assert_eq!(node.name.as_deref(), Some("Vault navigation"));
            assert_eq!(node.children.len(), 2);
            assert_eq!(node.children[0].name.as_deref(), Some("All items"));
            assert_eq!(node.children[0].description.as_deref(), Some("VAULT, 12"));
            assert_eq!(
                node.children[0].action_target.as_deref(),
                Some("filter:all")
            );
            assert!(node.children[0].state.selected);
            assert!(!node.children[1].state.selected);
            assert!(node.children.iter().all(|child| child.state.focusable));
        }
    }
}
