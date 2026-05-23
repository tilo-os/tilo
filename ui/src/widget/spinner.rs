//! Spinner — a circular indeterminate-progress indicator.
//!
//! Renders four dots orbiting a centre point at constant angular velocity
//! (one full rotation per [`ROTATION_PERIOD_MS`] ms). The leading dot is
//! painted at full accent-primary alpha; trailing dots fade through a
//! fixed alpha ramp so the head of the chain reads as "where the motion
//! is going". Animation is driven by wall-clock time captured at
//! construction (`start: Instant`) — there is no per-frame mutation, so
//! the widget is cheap to keep alive and safe to drop into any tree.
//!
//! The widget opts into [`Widget::needs_animation_frame`] permanently:
//! the host (`App`) polls this once per tick and schedules another
//! redraw while any visible spinner exists. That guarantees the dots
//! keep moving without external timer plumbing — and matches the
//! contract any spinner-on-screen test depends on.

use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};

use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

/// Default diameter (in CSS-like px). Picked to read clearly at the
/// concept's body text size (14 px line-height ≈ 20 px) while leaving
/// the dots large enough that the alpha ramp is legible.
pub const DEFAULT_SIZE_PX: f32 = 24.0;

/// One rotation every 1.5 seconds. Slow enough that the eye can follow
/// the leading dot, fast enough that the spinner reads as "active" at
/// a glance.
const ROTATION_PERIOD_MS: f64 = 1500.0;

/// Number of orbiting dots. Hard-coded; the tests pin this and the
/// alpha ramp must always have exactly this many entries.
const DOT_COUNT: usize = 4;

/// Per-dot alpha factors, indexed from the *leading* dot (index 0) to
/// the trailing dot (index `DOT_COUNT - 1`). The leading dot is full
/// opacity; trailing dots fade so the head of the chain is the
/// brightest point on the orbit.
const DOT_ALPHAS: [f32; DOT_COUNT] = [1.0, 0.75, 0.5, 0.25];

/// Fraction of `size` used as the dot radius. ~10 % of the diameter
/// keeps the dots visually distinct without overlapping at the
/// orbit radius.
const DOT_RADIUS_FRAC: f64 = 0.10;

/// Fraction of `size` used as the orbit radius (centre → dot centre).
/// 35 % leaves a healthy margin between the dot edge and the spinner
/// bounding box at the default dot radius.
const ORBIT_RADIUS_FRAC: f64 = 0.35;

/// Circular indeterminate-progress indicator. See module docs.
pub struct Spinner {
    /// Logical diameter in px. Drives both the layout size and the
    /// dot/orbit radii.
    size: f32,
    /// Wall-clock anchor for the rotation phase. Captured at
    /// construction so the spinner starts at a deterministic angle.
    start: Instant,
    /// Last computed bounding box. Updated in `apply_layout` and used
    /// to centre the orbit in `draw`.
    rect: Rect,
}

impl Spinner {
    /// Construct a default-size (24 px) spinner. The rotation starts
    /// from the wall-clock moment of construction, so two spinners
    /// instantiated in the same frame will be in phase.
    pub fn new() -> Self {
        Self::with_size(DEFAULT_SIZE_PX)
    }

    /// Construct a spinner with a custom diameter in CSS-like px.
    pub fn with_size(size: f32) -> Self {
        Self {
            size,
            start: Instant::now(),
            rect: Rect::ZERO,
        }
    }

    /// Logical diameter in px. Exposed for tests and debug overlays;
    /// not part of the public API contract beyond that.
    #[cfg(test)]
    pub(crate) fn size(&self) -> f32 {
        self.size
    }
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for Spinner {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(self.size),
                    height: Dimension::length(self.size),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        let size = self.size as f64;
        let dot_r = size * DOT_RADIUS_FRAC;
        let orbit_r = size * ORBIT_RADIUS_FRAC;
        let centre = self.rect.center();

        // Phase ∈ [0, 1) wraps once per ROTATION_PERIOD_MS. We use ms
        // rather than seconds so the modulo stays stable for very long-
        // lived spinners (Instant elapsed is u128-nanosecond precise;
        // ms keeps the divisor small).
        let elapsed_ms = self.start.elapsed().as_secs_f64() * 1000.0;
        let phase = (elapsed_ms / ROTATION_PERIOD_MS).fract();

        let accent = theme.tokens.colors.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX];

        // The leading dot sits at the current phase angle; trailing
        // dots trail behind at evenly spaced offsets so the chain
        // covers a quarter of the orbit at any time.
        for i in 0..DOT_COUNT {
            let angle_frac = phase - (i as f64) / (DOT_COUNT as f64);
            // Subtract 2π · angle_frac then offset by -π/2 so the
            // leading dot crosses the top of the orbit at phase = 0.
            // Negative direction is clockwise, matching what most
            // users expect a "loading" spinner to do.
            let theta = -std::f64::consts::TAU * angle_frac - std::f64::consts::FRAC_PI_2;
            let cx = centre.x + orbit_r * theta.cos();
            let cy = centre.y + orbit_r * theta.sin();
            let dot = Circle::new(Point::new(cx, cy), dot_r);
            let colour = accent.with_alpha_factor(DOT_ALPHAS[i]);
            scene.fill(vello::peniko::Fill::NonZero, transform, colour, None, &dot);
        }
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &str { "Spinner" }

    fn get_rect(&self) -> Rect { self.rect }

    /// Spinners are perpetually animating — every visible spinner has
    /// to keep pumping redraws so the dots actually move. The host
    /// (`App`) polls this and schedules another redraw while it's
    /// true; without the override the default would defer to children
    /// (which the spinner has none of) and return `false`, freezing
    /// the orbit at the first frame after instantiation.
    fn needs_animation_frame(&self) -> bool { true }

    /// Spinner has fixed `self.size` square layout — only the
    /// dot positions inside it change per frame. Sentinel
    /// `Rect::ZERO` so the first frame still computes layout.
    fn is_layout_dirty(&self) -> bool { self.rect == Rect::ZERO }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::PresenceCtx;
    use std::collections::HashMap;
    use taffy::prelude::TaffyMaxContent;
    use vello_encoding::DrawTag;

    /// `Spinner::new()` (and the `with_size` constructor when handed
    /// the default) must lay out as a 24×24 square. Pins the
    /// `DEFAULT_SIZE_PX` constant against the public contract — if a
    /// future change to the default needs to be deliberate, this test
    /// has to be touched at the same time.
    #[test]
    fn spinner_default_size_is_24_px() {
        // First sanity-check the field itself — covers the case where
        // the constructor wires the value through correctly.
        let s = Spinner::new();
        assert_eq!(s.size(), 24.0, "Spinner::new() must default to 24 px");

        // Then prove the layout pipeline honours that size. A floating
        // leaf with `Dimension::length(24)` resolves to a 24×24 rect
        // regardless of the parent's available space.
        let mut s = Spinner::new();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = s.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let root = taffy
            .new_with_children(Style::default(), &[node])
            .unwrap();
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        s.apply_layout(&taffy, node, Point::ZERO);

        assert_eq!(s.get_rect().width(), 24.0);
        assert_eq!(s.get_rect().height(), 24.0);
    }

    /// The host keeps pumping redraws only while at least one widget
    /// in the tree returns true from `needs_animation_frame`. The
    /// spinner's orbit can't animate without this — pin it so a
    /// future refactor that "cleans up" the override removes the only
    /// thing that keeps the dots moving in production.
    #[test]
    fn spinner_needs_animation_frame_always() {
        let s = Spinner::new();
        assert!(
            Widget::needs_animation_frame(&s),
            "Spinner must always request animation frames"
        );

        // Custom-size construction must not accidentally opt out
        // either — guards against a future field-level gate.
        let s = Spinner::with_size(48.0);
        assert!(
            Widget::needs_animation_frame(&s),
            "Spinner::with_size must also request animation frames"
        );
    }

    /// End-to-end scene check: a single `draw()` pass must emit
    /// exactly four `COLOR` draw-tags — one per orbiting dot. Every
    /// dot is a single `scene.fill(NonZero, ..., Circle)`, which the
    /// Vello encoder records as a single `DrawTag::COLOR` per call.
    /// Pinning the exact count guards against:
    ///   - A regression that draws a background ring (would inflate the
    ///     count).
    ///   - A regression that collapses the chain to fewer dots (would
    ///     deflate the count).
    ///   - A regression that paints text or icons inside the spinner
    ///     (would inflate the count via glyph fills).
    #[test]
    fn spinner_paints_four_dots() {
        let mut s = Spinner::new();
        // Stage a non-zero rect so the dots have an orbit to draw on.
        // We don't run the full taffy compute here — we only need the
        // rect to be where `draw` reads it from.
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = s.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let root = taffy
            .new_with_children(Style::default(), &[node])
            .unwrap();
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        s.apply_layout(&taffy, node, Point::ZERO);

        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        s.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let colour_fills = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();

        assert_eq!(
            colour_fills, DOT_COUNT,
            "Spinner must emit exactly {DOT_COUNT} colour fills — one per orbiting dot (got {colour_fills})"
        );
    }
}
