//! `Tooltip` primitive — a small label revealed after a short hover delay.
//!
//! `on_hover_enter`/`on_hover_leave`/`tick` drive the timing state so the
//! primitive is unit-testable without a winit event loop or a wall clock.

use crate::num::f32_of_f64;
use crate::time::{Duration, Instant};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::WindowEvent;

use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;

/// Middle of the 400-600ms comfort band: shows help promptly on a
/// deliberate hover without firing on a pointer merely passing through.
pub const HOVER_DWELL: Duration = Duration::from_millis(500);

/// A tooltip chip's spacing, every value a token (HIG 7b): the label
/// stands `s2` in from the chip's sides and `s1` from its top and bottom,
/// the chip hangs `s1` off its control (the tight relationship of a label
/// and the thing it names), stays the floating Panel margin
/// ([`super::floating_surface::viewport_margin`]) inside the window and is
/// never wider than a utility Panel, so a long hint wraps instead of
/// running off the window.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ChipMetrics {
    pad_x: f64,
    pad_y: f64,
    gap: f64,
    margin: f64,
    max_w: f64,
}

impl ChipMetrics {
    fn of(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        Self {
            pad_x: f64::from(spacing.s2),
            pad_y: f64::from(spacing.s1),
            gap: f64::from(spacing.rhythm.tight),
            margin: super::floating_surface::viewport_margin(theme),
            max_w: f64::from(theme.tokens.metrics.panel_width_utility),
        }
    }
}

/// The chip's size for `label`: wrapped `Caption` text plus the chip's pads.
pub fn chip_size(label: &str, text_renderer: &mut TextRenderer, theme: &Theme) -> (f64, f64) {
    let max_w = ChipMetrics::of(theme).max_w;
    chip_size_with_width(label, text_renderer, theme, max_w)
}

/// Measure the complete hint at the width its viewport can actually hold.
pub fn chip_size_in_viewport(
    label: &str,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    viewport: Rect,
) -> (f64, f64) {
    let m = ChipMetrics::of(theme);
    let width = (viewport.width() - 2.0 * m.margin).clamp(0.0, m.max_w);
    fit_size(
        &m,
        chip_size_with_width(label, text_renderer, theme, width),
        viewport,
    )
}

fn chip_size_with_width(
    label: &str,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    width: f64,
) -> (f64, f64) {
    let m = ChipMetrics::of(theme);
    let content_width = f32_of_f64((width - 2.0 * m.pad_x).max(0.0));
    let (tw, th) =
        text_renderer.measure_style(label, Some(content_width), &theme.text(TextRole::Caption));
    let w = (f64::from(tw) + 2.0 * m.pad_x).min(width);
    let h = f64::from(th) + 2.0 * m.pad_y;
    (w, h)
}

fn fit_size(m: &ChipMetrics, size: (f64, f64), viewport: Rect) -> (f64, f64) {
    (
        size.0.min((viewport.width() - 2.0 * m.margin).max(0.0)),
        size.1.min((viewport.height() - 2.0 * m.margin).max(0.0)),
    )
}

/// Which side of its control a tooltip prefers. A control in a row has
/// its neighbours beside it, so its chip goes below; one in a vertical
/// rail has them above and below, so its chip goes beside it, where it
/// hides none of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TooltipSide {
    #[default]
    Below,
    /// After the control in the reading direction, before it when that
    /// would spill past the window's edge.
    Beside,
}

/// [`chip_rect`] on the preferred `side` of the anchor.
pub fn chip_rect_on(
    theme: &Theme,
    side: TooltipSide,
    anchor: Rect,
    size: (f64, f64),
    viewport: Rect,
) -> Rect {
    match side {
        TooltipSide::Below => chip_rect(theme, anchor, size, viewport),
        TooltipSide::Beside => chip_rect_beside(&ChipMetrics::of(theme), anchor, size, viewport),
    }
}

/// Centred on the anchor vertically, just after it; flipped before it
/// when that would spill past the right edge, clamped when neither fits.
fn chip_rect_beside(m: &ChipMetrics, anchor: Rect, size: (f64, f64), viewport: Rect) -> Rect {
    let (w, h) = fit_size(m, size, viewport);
    let ChipMetrics { gap, margin, .. } = *m;
    let min_y = viewport.y0 + margin;
    let max_y = (viewport.y1 - margin - h).max(min_y);
    let y0 = (anchor.center().y - h / 2.0).clamp(min_y, max_y);
    let after = anchor.x1 + gap;
    let x0 = if after + w <= viewport.x1 - margin {
        after
    } else {
        let before = anchor.x0 - gap - w;
        if before >= viewport.x0 + margin {
            before
        } else {
            (viewport.x1 - margin - w).max(viewport.x0 + margin)
        }
    };
    Rect::new(x0, y0, x0 + w, y0 + h)
}

/// Preferred placement is centred just below the anchor; flips above if
/// that would spill past the bottom edge, and clamps to the bottom edge if
/// neither side has room. Clamped horizontally too, so a control at the
/// far edge gets a chip that hugs it instead of painting off-screen.
pub fn chip_rect(theme: &Theme, anchor: Rect, size: (f64, f64), viewport: Rect) -> Rect {
    let m = ChipMetrics::of(theme);
    let (w, h) = fit_size(&m, size, viewport);
    let ChipMetrics { gap, margin, .. } = m;
    // Horizontal: centre under the anchor, then clamp within the viewport.
    let min_x = viewport.x0 + margin;
    let max_x = (viewport.x1 - margin - w).max(min_x);
    let x0 = (anchor.center().x - w / 2.0).clamp(min_x, max_x);
    // Vertical: below the anchor; flip above when it would overflow the
    // bottom, and fall back to a bottom clamp when neither side fits.
    let below = anchor.y1 + gap;
    let y0 = if below + h <= viewport.y1 - margin {
        below
    } else {
        let above = anchor.y0 - gap - h;
        if above >= viewport.y0 + margin {
            above
        } else {
            (viewport.y1 - margin - h).max(viewport.y0 + margin)
        }
    };
    Rect::new(x0, y0, x0 + w, y0 + h)
}

/// The floating material every floating surface is made of (blurred where
/// the renderer runs the backdrop pass, the opaque panel ground otherwise),
/// with the panel edge, at the control radius: a tooltip is a small
/// floating Panel, not a card.
pub fn draw_chip(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
) {
    draw_chip_marked(scene, transform, text_renderer, theme, rect, (label, None));
}

/// The room a person's colour dot takes before a chip's label: its
/// diameter, and the gap after it.
fn mark_room(theme: &Theme) -> (f64, f64) {
    let rhythm = theme.tokens.spacing.rhythm;
    (f64::from(rhythm.related), f64::from(rhythm.tight))
}

/// [`draw_chip`] with a coloured dot before the label when `mark` is set.
fn draw_chip_marked(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    (label, mark): (&str, Option<vello::peniko::Color>),
) {
    let radius = f64::from(theme.tokens.radii.control);

    // The elevation token's own blur, following the chip's radius — the same
    // painter every other raised surface uses, rather than two hand-offset
    // translucent copies approximating it.
    crate::surface::shadow(
        scene,
        transform,
        rect,
        radius,
        &theme.tokens.elevation.floating,
    );

    // The Panel row: material, panel edge and, on a dark material, the
    // inner light edge. The chip is centred under its anchor and so usually
    // lands on a half pixel; snap the painted box to the grid or the
    // hairline smears across two columns at half strength.
    let snapped = Rect::new(
        rect.x0.round(),
        rect.y0.round(),
        rect.x1.round(),
        rect.y1.round(),
    );
    crate::surface::panel(scene, transform, snapped, radius, theme);
    let on_panel = theme.on_ground(crate::chrome::panel_ground(theme));

    let text_style = theme.text(TextRole::Caption);
    // Centre on the measured line, not the font size: the chip's box is
    // sized from the font size, so the label otherwise sits low by half the
    // face's ascent/descent difference.
    let m = ChipMetrics::of(theme);
    let (mark_px, mark_gap) = mark_room(theme);
    let lead = mark.map_or(0.0, |color| {
        let centre = vello::kurbo::Point::new(rect.x0 + m.pad_x + mark_px / 2.0, rect.center().y);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            color,
            None,
            &vello::kurbo::Circle::new(centre, mark_px / 2.0),
        );
        mark_px + mark_gap
    });
    let content = Rect::new(
        rect.x0 + m.pad_x + lead,
        rect.y0 + m.pad_y,
        rect.x1 - m.pad_x,
        rect.y1 - m.pad_y,
    );
    if content.width() <= 0.0 || content.height() <= 0.0 {
        return;
    }
    let content_width = f32_of_f64(content.width());
    let (_, label_h) = text_renderer.measure_style(label, Some(content_width), &text_style);
    scene.push_layer(
        vello::peniko::Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        transform,
        &content,
    );
    let text_y = rect.y0 + (rect.height() - f64::from(label_h)) / 2.0;
    text_renderer.draw_style(
        scene,
        label,
        on_panel.tokens.colors.text_primary,
        transform.pre_translate((content.x0, text_y).into()),
        Some(content_width),
        &text_style,
    );
    scene.pop_layer();
}

/// Composes [`chip_size`], [`chip_rect`] and [`draw_chip`] — the one call
/// [`TooltipHost`] uses to render a tooltip.
pub fn draw_hover_tooltip(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    anchor: Rect,
    label: &str,
    viewport: Rect,
) {
    draw_hover_tooltip_on(
        scene,
        transform,
        text_renderer,
        theme,
        (anchor, TooltipSide::Below),
        label,
        viewport,
    );
}

/// [`draw_hover_tooltip`] on the side of its anchor the control prefers.
pub fn draw_hover_tooltip_on(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    anchor: (Rect, TooltipSide),
    label: &str,
    viewport: Rect,
) {
    let rect = hover_chip_rect_on(text_renderer, theme, anchor, label, viewport);
    draw_chip(scene, transform, text_renderer, theme, rect, label);
}

/// A tooltip naming a person: their presence `color` as a dot before
/// `label`, the way presence marks them everywhere else, on the side of
/// `anchor` the control prefers.
pub fn draw_person_tooltip_on(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    anchor: (Rect, TooltipSide),
    (label, color): (&str, vello::peniko::Color),
    viewport: Rect,
) {
    let (width, height) = chip_size_in_viewport(label, text_renderer, theme, viewport);
    let (mark_px, mark_gap) = mark_room(theme);
    let size = (width + mark_px + mark_gap, height);
    let rect = chip_rect_on(theme, anchor.1, anchor.0, size, viewport);
    draw_chip_marked(
        scene,
        transform,
        text_renderer,
        theme,
        rect,
        (label, Some(color)),
    );
}

/// Where [`draw_hover_tooltip_on`] places the chip for `label`.
pub fn hover_chip_rect_on(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    anchor: (Rect, TooltipSide),
    label: &str,
    viewport: Rect,
) -> Rect {
    let size = chip_size_in_viewport(label, text_renderer, theme, viewport);
    chip_rect_on(theme, anchor.1, anchor.0, size, viewport)
}

/// Every pixel [`draw_chip`] can paint for a chip at `rect`: the chip, its
/// edge and snapping, and the floating shadow under it.
pub fn chip_paint_bounds(rect: Rect, theme: &Theme) -> Rect {
    // The chip snaps to whole pixels and strokes its edge on the outline:
    // two pixels cover both, antialiasing included.
    let chip = rect.inflate(2.0, 2.0);
    crate::surface::shadow_bounds(rect, &theme.tokens.elevation.floating)
        .map_or(chip, |shadow| shadow.union(chip))
}

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

pub const DEFAULT_DELAY: Duration = Duration::from_millis(600);

pub struct Tooltip {
    label: String,
    delay: Duration,
    /// `Some(accumulated)` while a hover is in progress, `None` when the
    /// pointer is not over the anchor.
    elapsed: Option<Duration>,
    is_visible: bool,
    rect: Rect,
    /// When set, `handle_event` self-routes hover enter/leave. `Rect::ZERO`
    /// leaves the widget driven purely by the explicit `on_hover_*`/`tick`
    /// API (the test path).
    anchor: Rect,
    hover_since: Option<Instant>,
}

impl std::fmt::Debug for Tooltip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tooltip")
            .field("label", &self.label)
            .field("delay", &self.delay)
            .field("elapsed", &self.elapsed)
            .field("is_visible", &self.is_visible)
            .field("rect", &self.rect)
            .field("anchor", &self.anchor)
            .field("hover_since", &self.hover_since)
            .finish()
    }
}

impl Tooltip {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            delay: DEFAULT_DELAY,
            elapsed: None,
            is_visible: false,
            rect: Rect::ZERO,
            anchor: Rect::ZERO,
            hover_since: None,
        }
    }

    #[must_use]
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Enables self-routing in `handle_event`.
    #[must_use]
    pub fn with_anchor(mut self, anchor: Rect) -> Self {
        self.anchor = anchor;
        self
    }

    pub fn set_anchor(&mut self, anchor: Rect) {
        self.anchor = anchor;
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn delay(&self) -> Duration {
        self.delay
    }

    pub fn is_visible(&self) -> bool {
        self.is_visible
    }

    /// `initial` seeds the accumulator so a pointer already hovering can be
    /// resumed without losing that elapsed duration (tests pass `Duration::ZERO`).
    pub fn on_hover_enter(&mut self, initial: Duration) {
        self.elapsed = Some(initial);
        self.is_visible = initial >= self.delay;
    }

    pub fn on_hover_leave(&mut self) {
        self.elapsed = None;
        self.is_visible = false;
    }

    pub fn tick(&mut self, dt: Duration) {
        let Some(e) = self.elapsed.as_mut() else {
            return;
        };
        *e = e.saturating_add(dt);
        if *e >= self.delay {
            self.is_visible = true;
        }
    }
}

const TOOLTIP_WIDTH: f64 = 220.0;
const TOOLTIP_HEIGHT: f64 = 28.0;

impl Widget for Tooltip {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let (w, h) = if self.is_visible {
            (f32_of_f64(TOOLTIP_WIDTH), f32_of_f64(TOOLTIP_HEIGHT))
        } else {
            (0.0, 0.0)
        };
        let style = Style {
            size: Size::from_lengths(w, h),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(since) = self.hover_since
            && !self.is_visible
            && since.elapsed() >= self.delay
        {
            self.is_visible = true;
        }
        if !self.is_visible {
            return;
        }

        // Generous fallback so an unclamped chip still renders in bare test
        // harnesses that never push a viewport.
        let viewport = presence
            .viewport
            .get()
            .unwrap_or(Rect::new(0.0, 0.0, 100_000.0, 100_000.0));
        let size = chip_size_in_viewport(&self.label, text_renderer, theme, viewport);
        if self.anchor != Rect::ZERO {
            self.rect = chip_rect(theme, self.anchor, size, viewport);
        }
        draw_chip(
            scene,
            transform,
            text_renderer,
            theme,
            self.rect,
            &self.label,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        // Without an anchor the widget stays driven by the explicit
        // on_hover_*/tick API.
        if self.anchor == Rect::ZERO {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = ctx.logical(position);
                let inside = self.anchor.contains(p);
                if inside && self.hover_since.is_none() {
                    self.on_hover_enter(Duration::ZERO);
                    self.hover_since = Some(crate::time::now());
                } else if !inside && self.hover_since.is_some() {
                    self.on_hover_leave();
                    self.hover_since = None;
                }
            }
            // A click, scroll or keypress dismisses immediately — the user
            // has moved on to acting, and a lingering chip would obscure it.
            WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::KeyboardInput { .. }
                if self.hover_since.is_some() =>
            {
                self.on_hover_leave();
                self.hover_since = None;
            }
            _ => {}
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.hover_since.is_some() && !self.is_visible
    }

    fn name(&self) -> &'static str {
        "Tooltip"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Not on screen until the reveal delay elapses, so hidden until then.
        Some(
            AccessNode::new(Role::Tooltip)
                .with_name(self.label.clone())
                .with_state(AccessState {
                    hidden: !self.is_visible,
                    ..Default::default()
                }),
        )
    }
}

#[derive(Clone, Debug)]
struct HoverTarget {
    /// May be a fluent key, resolved by the caller of [`TooltipHost::visible`].
    label: String,
    anchor: Rect,
    since: Instant,
}

/// One controller owned by [`crate::app::App`] — not one tooltip widget per
/// control — is what makes tooltips work for every widget exposing
/// [`Widget::tooltip`] with zero per-app
/// wiring, and guarantees at most one chip on screen. Timing is wall-clock
/// through an injected `now: Instant`, so it's unit-testable.
pub struct TooltipHost {
    dwell: Duration,
    target: Option<HoverTarget>,
    visible: bool,
}

impl Default for TooltipHost {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for TooltipHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TooltipHost")
            .field("dwell", &self.dwell)
            .field("visible", &self.visible)
            .field("has_target", &self.target.is_some())
            .finish()
    }
}

impl TooltipHost {
    pub fn new() -> Self {
        Self {
            dwell: HOVER_DWELL,
            target: None,
            visible: false,
        }
    }

    #[must_use]
    pub fn with_dwell(mut self, dwell: Duration) -> Self {
        self.dwell = dwell;
        self
    }

    fn recompute_visible(&mut self, now: Instant) {
        if !self.visible
            && let Some(t) = &self.target
            && now.saturating_duration_since(t.since) >= self.dwell
        {
            self.visible = true;
        }
    }

    /// Re-hovering the *same* region keeps the accumulated dwell (so a
    /// jittering pointer still reveals); moving to a different region
    /// restarts it. Returns `true` when the caller should request a redraw.
    pub fn on_pointer(&mut self, target: Option<(Rect, String)>, now: Instant) -> bool {
        if let Some((anchor, label)) = target {
            let same = self
                .target
                .as_ref()
                .is_some_and(|t| t.label == label && t.anchor == anchor);
            if same {
                let was = self.visible;
                self.recompute_visible(now);
                self.visible != was
            } else {
                self.target = Some(HoverTarget {
                    label,
                    anchor,
                    since: now,
                });
                self.visible = false;
                self.recompute_visible(now);
                // A new target always warrants a redraw: it may clear a
                // previously-shown chip.
                true
            }
        } else {
            let had = self.target.is_some() || self.visible;
            self.target = None;
            self.visible = false;
            had
        }
    }

    /// What the region under a shown tooltip says now: a new label for the
    /// same region replaces the shown one without restarting the dwell; the
    /// region gone or moved hides it.
    pub fn refresh(&mut self, target: Option<(Rect, String)>) {
        let Some(current) = &mut self.target else {
            return;
        };
        match target {
            Some((anchor, label)) if anchor == current.anchor => current.label = label,
            _ => {
                self.target = None;
                self.visible = false;
            }
        }
    }

    /// Returns whether anything was showing/pending, so the caller can
    /// request a redraw to clear the chip.
    pub fn dismiss(&mut self) -> bool {
        let had = self.target.is_some() || self.visible;
        self.target = None;
        self.visible = false;
        had
    }

    /// Returns `true` on the tick it *becomes* visible.
    pub fn tick(&mut self, now: Instant) -> bool {
        let was = self.visible;
        self.recompute_visible(now);
        self.visible && !was
    }

    /// The label is the raw string the region exposed; the caller resolves
    /// i18n and renders via [`draw_hover_tooltip`].
    pub fn visible(&self) -> Option<(&str, Rect)> {
        if self.visible {
            self.target.as_ref().map(|t| (t.label.as_str(), t.anchor))
        } else {
            None
        }
    }

    pub fn needs_frame(&self) -> bool {
        self.target.is_some() && !self.visible
    }
}

#[cfg(test)]
mod host_tests {
    use super::*;

    fn tip(label: &str) -> (Rect, String) {
        (Rect::new(0.0, 0.0, 40.0, 40.0), label.to_string())
    }

    #[test]
    fn a_shown_tooltip_reads_its_region_again() {
        let mut host = TooltipHost::new().with_dwell(Duration::ZERO);
        let now = crate::time::now();
        host.on_pointer(Some(tip("Hide layer")), now);
        host.tick(now);
        host.refresh(Some(tip("Show layer")));
        assert_eq!(host.visible().map(|(label, _)| label), Some("Show layer"));
        host.refresh(None);
        assert!(host.visible().is_none(), "the region is gone");
    }

    #[test]
    fn tooltip_host_dwell_reveals_restarts_and_dismisses() {
        // Case `hovering_past_dwell_reveals_then_leaving_hides`.
        {
            let mut h = TooltipHost::new().with_dwell(Duration::from_millis(500));
            let t0 = crate::time::now();
            assert!(
                h.on_pointer(Some(tip("Search")), t0),
                "new target changes state"
            );
            assert!(h.visible().is_none(), "not shown before the dwell elapses");
            assert!(h.needs_frame(), "a pending reveal pumps frames");

            // A tick short of the dwell keeps it hidden.
            h.tick(t0 + Duration::from_millis(300));
            assert!(h.visible().is_none());

            // Past the dwell it reveals — with the region's label + anchor.
            assert!(h.tick(t0 + Duration::from_millis(600)), "becomes visible");
            let (label, anchor) = h.visible().expect("revealed after dwell");
            assert_eq!(label, "Search");
            assert_eq!(anchor, Rect::new(0.0, 0.0, 40.0, 40.0));
            assert!(!h.needs_frame(), "no frame pump once shown");

            // Leaving the region (pointer over nothing) hides it immediately.
            assert!(h.on_pointer(None, t0 + Duration::from_millis(650)));
            assert!(h.visible().is_none(), "hides on leave");
        }

        // Case `click_scroll_or_key_dismisses_a_shown_tooltip`.
        {
            let mut h = TooltipHost::new().with_dwell(Duration::ZERO);
            let t0 = crate::time::now();
            h.on_pointer(Some(tip("Zoom in")), t0);
            assert!(h.visible().is_some(), "zero-dwell shows on hover");
            assert!(h.dismiss(), "dismiss reports it was showing");
            assert!(h.visible().is_none(), "click/scroll/key hides it");
            // Staying on the same region after a dismiss does not re-show until a
            // fresh pointer move re-targets it.
            assert!(h.visible().is_none());
        }

        // Case `moving_between_regions_restarts_the_dwell`.
        {
            let mut h = TooltipHost::new().with_dwell(Duration::from_millis(500));
            let t0 = crate::time::now();
            h.on_pointer(Some(tip("Back")), t0);
            // Almost revealed on the first region…
            h.tick(t0 + Duration::from_millis(450));
            assert!(h.visible().is_none());
            // …then the pointer crosses to a different region: the dwell restarts,
            // so a tick that *would* have revealed the first one does not reveal
            // the second.
            let other = Some((Rect::new(50.0, 0.0, 90.0, 40.0), "Forward".to_string()));
            assert!(h.on_pointer(other, t0 + Duration::from_millis(460)));
            h.tick(t0 + Duration::from_millis(600));
            assert!(h.visible().is_none(), "dwell restarted on the new region");
            // Only after a full fresh dwell on the second region does it show.
            h.tick(t0 + Duration::from_millis(960));
            assert_eq!(h.visible().map(|(l, _)| l), Some("Forward"));
        }

        // Case `jitter_within_one_region_keeps_the_dwell`.
        {
            let mut h = TooltipHost::new().with_dwell(Duration::from_millis(500));
            let t0 = crate::time::now();
            h.on_pointer(Some(tip("Search")), t0);
            // The pointer keeps reporting the same region (a stable rect+label)
            // as it jitters — the accumulated dwell must not reset.
            h.on_pointer(Some(tip("Search")), t0 + Duration::from_millis(200));
            h.on_pointer(Some(tip("Search")), t0 + Duration::from_millis(400));
            assert!(
                h.on_pointer(Some(tip("Search")), t0 + Duration::from_millis(550)),
                "crossing the threshold on a jitter move flips to visible"
            );
            assert_eq!(h.visible().map(|(l, _)| l), Some("Search"));
        }
    }

    #[test]
    fn a_rail_chip_sits_beside_its_tool_and_covers_no_neighbour() {
        let theme = Theme::dark();
        let vp = Rect::new(0.0, 0.0, 800.0, 600.0);
        let size = (120.0, 24.0);
        // Three tools stacked in a rail at the left edge.
        let tool = Rect::new(8.0, 100.0, 48.0, 140.0);
        let next = Rect::new(8.0, 144.0, 48.0, 184.0);
        let r = chip_rect_on(&theme, TooltipSide::Beside, tool, size, vp);
        assert!(r.x0 >= tool.x1, "after the tool: {r:?}");
        assert!(r.intersect(next).area() <= 0.0, "covers the next tool");
        assert!(
            (r.center().y - tool.center().y).abs() < 0.01,
            "level with it"
        );
        // A rail at the right edge puts it before the tool instead.
        let right = Rect::new(752.0, 100.0, 792.0, 140.0);
        let r = chip_rect_on(&theme, TooltipSide::Beside, right, size, vp);
        assert!(r.x1 <= right.x0, "before the tool: {r:?}");
        // Below stays the default placement.
        assert_eq!(
            chip_rect_on(&theme, TooltipSide::Below, tool, size, vp),
            chip_rect(&theme, tool, size, vp)
        );
    }

    #[test]
    fn chip_rect_flips_above_and_clamps_horizontally() {
        // A 800x600 viewport with a chip anchored near the bottom-right.
        let theme = Theme::dark();
        let vp = Rect::new(0.0, 0.0, 800.0, 600.0);
        let size = (120.0, 24.0);

        // Anchor near the bottom edge → the chip flips above the anchor.
        let bottom_anchor = Rect::new(400.0, 580.0, 440.0, 596.0);
        let r = chip_rect(&theme, bottom_anchor, size, vp);
        assert!(
            r.y1 <= bottom_anchor.y0,
            "flips above when it would overflow the bottom"
        );

        // Anchor at the far right → the chip is clamped inside the viewport.
        let right_anchor = Rect::new(790.0, 10.0, 800.0, 30.0);
        let r = chip_rect(&theme, right_anchor, size, vp);
        assert!(
            r.x1 <= vp.x1 - ChipMetrics::of(&theme).margin + 0.01,
            "clamped to the right edge"
        );
        assert!(r.x0 >= vp.x0, "never off the left edge");

        // Anchor at the far left → clamped to the left margin.
        let left_anchor = Rect::new(0.0, 10.0, 10.0, 30.0);
        let r = chip_rect(&theme, left_anchor, size, vp);
        assert!(
            r.x0 >= vp.x0 + ChipMetrics::of(&theme).margin - 0.01,
            "clamped to the left margin"
        );

        // A roomy central anchor keeps the chip below and centred.
        let center = Rect::new(390.0, 100.0, 410.0, 120.0);
        let r = chip_rect(&theme, center, size, vp);
        assert!(r.y0 >= center.y1, "placed below when there is room");
        assert!(
            (r.center().x - center.center().x).abs() < 0.5,
            "centred under the anchor"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Point;

    fn feed(t: &mut Tooltip, p: Point) {
        let ev = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: (p.x, p.y).into(),
        };
        crate::testing::dispatch(t, &ev);
    }

    #[test]
    fn anchored_tooltip_reveals_after_delay_and_is_inert_without_an_anchor() {
        // Case `hovering_anchor_enters_and_leaving_dismisses`.
        {
            // Zero delay → reveal is immediate on enter (on_hover_enter flips it).
            let mut t = Tooltip::new("Save")
                .with_delay(Duration::ZERO)
                .with_anchor(Rect::new(0.0, 0.0, 100.0, 30.0));
            assert!(!t.is_visible());
            feed(&mut t, Point::new(50.0, 15.0)); // inside anchor
            assert!(t.is_visible(), "zero-delay tooltip shows on hover-enter");
            feed(&mut t, Point::new(500.0, 500.0)); // outside
            assert!(!t.is_visible(), "hides on hover-leave");
        }

        // Case `delayed_tooltip_requests_frames_while_pending`.
        {
            let mut t = Tooltip::new("Later")
                .with_delay(Duration::from_millis(600))
                .with_anchor(Rect::new(0.0, 0.0, 80.0, 24.0));
            assert!(!t.needs_animation_frame());
            feed(&mut t, Point::new(10.0, 10.0)); // enter
            // Pending (not yet visible) → host must keep pumping frames.
            assert!(t.needs_animation_frame());
            assert!(!t.is_visible());
        }

        // Case `accessibility_reports_tooltip_role_label_and_hidden`.
        {
            use crate::a11y::Role;
            // A freshly-built tooltip is not yet visible → hidden from the tree.
            let mut t = Tooltip::new("Save").with_delay(Duration::ZERO);
            let node = Widget::accessibility(&t).expect("tooltip is not presentational");
            assert_eq!(node.role, Role::Tooltip);
            assert_eq!(node.name.as_deref(), Some("Save"));
            assert!(node.state.hidden, "an unrevealed tooltip is hidden");

            // Once revealed it is no longer hidden.
            t.on_hover_enter(Duration::ZERO);
            assert!(t.is_visible());
            let node = Widget::accessibility(&t).unwrap();
            assert!(
                !node.state.hidden,
                "a revealed tooltip is visible to the AT"
            );
        }

        // Case `without_anchor_handle_event_is_inert`.
        {
            // Back-compat: no anchor → explicit API still drives it.
            let mut t = Tooltip::new("x");
            feed(&mut t, Point::new(5.0, 5.0));
            assert!(!t.is_visible());
            t.on_hover_enter(Duration::ZERO);
            t.tick(Duration::from_millis(700));
            assert!(t.is_visible());
        }
    }
    #[test]
    fn tooltip_long_hint_wraps_inside_the_scaled_viewport() {
        let label = "Close applications and end this session, then return to the login screen.";
        for (theme, width) in [
            (Theme::dark(), 800.0),
            (Theme::light(), 280.0),
            (Theme::dark().with_text_scale(2.0), 320.0),
            (Theme::high_contrast().with_ui_scale(1.5), 360.0),
        ] {
            let viewport = Rect::new(0.0, 0.0, width, 480.0);
            let mut tip = Tooltip::new(label)
                .with_delay(Duration::ZERO)
                .with_anchor(Rect::new(width - 60.0, 100.0, width - 20.0, 132.0));
            let pointer = tip.anchor.center();
            feed(&mut tip, pointer);
            let presence = PresenceCtx::default();
            presence.viewport.set(Some(viewport));
            let mut text = TextRenderer::new();
            tip.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut text,
                &theme,
                &presence,
            );
            let rect = tip.get_rect();
            assert!(
                viewport.contains_rect(rect),
                "tooltip exceeds viewport: {rect:?}"
            );
            let (_, height) = text.measure_style(
                label,
                Some(f32_of_f64(
                    (rect.width() - 2.0 * ChipMetrics::of(&theme).pad_x).max(0.0),
                )),
                &theme.text(TextRole::Caption),
            );
            assert!(
                rect.height() + 0.01 >= f64::from(height) + 2.0 * ChipMetrics::of(&theme).pad_y,
                "complete wrapped hint must fit its painted box: {rect:?}, text height {height}"
            );
            assert_eq!(
                tip.accessibility()
                    .expect("tooltip semantics")
                    .name
                    .as_deref(),
                Some(label)
            );
        }
    }
}
