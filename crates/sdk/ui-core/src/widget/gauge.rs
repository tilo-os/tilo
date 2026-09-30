//! Gauges — [`RingGauge`] (a circular arc gauge with a centred label) and
//! [`BarGauge`] (a labelled horizontal bar). Both take a `0..1` fraction
//! and pick their fill colour from a set of [`GaugeThresholds`] so a value
//! crossing into the warning / danger band recolours automatically.
//!
//! The ring sweeps clockwise from 12 o'clock (`-π/2`) using a round-capped
//! `kurbo::Arc` over a faint full-circle track; the bar fills a rounded
//! track left-to-right. Track, label and threshold colours all come from
//! the [`Theme`] — nothing is hard-coded.

use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, TAU};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Arc, Cap, Circle, Join, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{Brush, Color, Fill};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_metrics::renderer_type_size;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Alpha of a gauge's unfilled track, derived against the raised surface
/// a gauge sits on (HIG §9.5): a declared border at any alpha is three
/// shades off that surface on the dark palette and the track vanishes.
const TRACK_ALPHA: u8 = 40;
const BAR_TRACK_H: f64 = 8.0;

/// The unfilled track colour on the theme's raised surface.
fn track_color(theme: &Theme, alpha: u8) -> Color {
    crate::widget::data_table::tier_on(theme.tokens.colors.surfaces.raised, alpha)
}

/// Paint options shared by [`RingGauge`] and [`BarGauge`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GaugeStyle {
    /// Alpha of the unfilled track against the raised surface. Denser
    /// dashboards raise it so a thin track still reads next to its fill.
    pub track_alpha: u8,
    /// Set a bar gauge's label and value at the `xs` step instead of `sm`,
    /// for a dense grid of small gauges.
    pub compact_labels: bool,
}

impl Default for GaugeStyle {
    fn default() -> Self {
        Self {
            track_alpha: TRACK_ALPHA,
            compact_labels: false,
        }
    }
}

impl GaugeStyle {
    #[must_use]
    pub fn with_track_alpha(mut self, alpha: u8) -> Self {
        self.track_alpha = alpha;
        self
    }

    #[must_use]
    pub fn compact(mut self) -> Self {
        self.compact_labels = true;
        self
    }
}

/// What an immediate-mode gauge paints: how full it is, in which colour,
/// and the text around it. Either string may be empty.
#[derive(Clone, Copy, Debug)]
pub struct GaugeReading<'a> {
    /// Fill fraction, clamped to `0..1` when painted.
    pub frac: f32,
    pub color: Color,
    /// What is measured — a ring's dim caption, a bar's top-left label.
    pub label: &'a str,
    /// The readout — a ring's prominent centre text, a bar's right-aligned value.
    pub value: &'a str,
}

/// Rounds a `0..=1` fraction to a whole percentage for the accessibility
/// value string. Always within `0..=100`, far inside `i64` range.
#[allow(
    clippy::cast_possible_truncation,
    reason = "frac is clamped to 0..1 immediately above, so the percentage is always within 0..=100"
)]
fn pct(frac: f32) -> i64 {
    (frac.clamp(0.0, 1.0) * 100.0).round() as i64
}

/// Fraction thresholds (inclusive lower bounds) at which a gauge switches
/// from success → warning → danger. The default mirrors the dashboard
/// convention: `>= 0.85` danger, `>= 0.6` warning, else success.
#[derive(Clone, Copy, Debug)]
pub struct GaugeThresholds {
    pub warning: f32,
    pub danger: f32,
}

impl Default for GaugeThresholds {
    fn default() -> Self {
        Self {
            warning: 0.6,
            danger: 0.85,
        }
    }
}

impl GaugeThresholds {
    pub fn color(&self, theme: &Theme, frac: f32) -> Color {
        let s = &theme.tokens.colors.semantic;
        if frac >= self.danger {
            s.danger
        } else if frac >= self.warning {
            s.warning
        } else {
            s.success
        }
    }
}

/// How a gauge sources its fill colour.
#[derive(Clone, Copy, Debug)]
enum Fillcolor {
    /// Resolve via [`GaugeThresholds`] against the value (default).
    Threshold(GaugeThresholds),
    /// A fixed caller-supplied colour, ignoring thresholds.
    Fixed(Color),
}

pub struct RingGauge {
    frac: f32,
    fill: Fillcolor,
    big: String,
    small: String,
    /// Box edge length; the ring is centred and inset within it.
    size: f32,
    /// Arc thickness; `0.0` ⇒ derived from the radius.
    thickness: f64,
    style: GaugeStyle,
    rect: Rect,
}

impl RingGauge {
    /// `frac` is clamped to `0..1`.
    pub fn new(frac: f32) -> Self {
        Self {
            frac: frac.clamp(0.0, 1.0),
            fill: Fillcolor::Threshold(GaugeThresholds::default()),
            big: String::new(),
            small: String::new(),
            size: 96.0,
            thickness: 0.0,
            style: GaugeStyle::default(),
            rect: Rect::ZERO,
        }
    }

    /// `big` (prominent, bold) over `small` (dim caption). Either may be
    /// empty.
    #[must_use]
    pub fn with_label(mut self, big: impl Into<String>, small: impl Into<String>) -> Self {
        self.big = big.into();
        self.small = small.into();
        self
    }

    #[must_use]
    pub fn with_thresholds(mut self, thresholds: GaugeThresholds) -> Self {
        self.fill = Fillcolor::Threshold(thresholds);
        self
    }

    /// Bypasses the threshold bands.
    #[must_use]
    pub fn with_color(mut self, color: Color) -> Self {
        self.fill = Fillcolor::Fixed(color);
        self
    }

    #[must_use]
    pub fn with_size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    #[must_use]
    pub fn with_style(mut self, style: GaugeStyle) -> Self {
        self.style = style;
        self
    }

    pub fn style(&self) -> &GaugeStyle {
        &self.style
    }

    pub fn style_mut(&mut self) -> &mut GaugeStyle {
        &mut self.style
    }

    /// Otherwise `radius * 0.16`.
    #[must_use]
    pub fn with_thickness(mut self, thickness: f64) -> Self {
        self.thickness = thickness;
        self
    }

    /// Update the fraction (clamped to `0..1`).
    pub fn set_fraction(&mut self, frac: f32) {
        self.frac = frac.clamp(0.0, 1.0);
    }

    fn resolve_color(&self, theme: &Theme) -> Color {
        match self.fill {
            Fillcolor::Threshold(t) => t.color(theme, self.frac),
            Fillcolor::Fixed(c) => c,
        }
    }

    /// Paint a ring of `thickness` along `ring`, filled to `reading.frac`,
    /// with `reading.value` over `reading.label` at its centre.
    pub fn draw_at(
        ctx: &mut PaintCtx<'_>,
        ring: Circle,
        thickness: f64,
        reading: &GaugeReading<'_>,
    ) {
        Self::draw_styled(ctx, ring, thickness, reading, &GaugeStyle::default());
    }

    /// [`Self::draw_at`] with an explicit [`GaugeStyle`].
    pub fn draw_styled(
        ctx: &mut PaintCtx<'_>,
        ring: Circle,
        thickness: f64,
        reading: &GaugeReading<'_>,
        style: &GaugeStyle,
    ) {
        if ring.radius <= 0.0 {
            return;
        }
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let (center, radius) = (ring.center, ring.radius);
        scene.stroke(
            &Stroke::new(thickness),
            transform,
            track_color(theme, style.track_alpha),
            None,
            &ring,
        );

        let frac = f64::from(reading.frac.clamp(0.0, 1.0));
        if frac > 0.0 {
            let arc = Arc::new(center, (radius, radius), -FRAC_PI_2, frac * TAU, 0.0);
            let stroke = Stroke::new(thickness)
                .with_caps(Cap::Round)
                .with_join(Join::Round);
            scene.stroke(&stroke, transform, reading.color, None, &arc);
        }

        let tokens = &theme.tokens;
        let (big, small) = (reading.value, reading.label);
        if !big.is_empty() {
            let size = renderer_type_size(text_renderer, tokens.typography.scale.xl);
            let (bw, bh) = text_renderer.measure_weighted(big, size, None, TextWeight::Bold);
            let bx = center.x - f64::from(bw) / 2.0;
            let by = center.y - f64::from(bh) / 2.0 - if small.is_empty() { 0.0 } else { 4.0 };
            text_renderer.draw_text_weighted(
                scene,
                big,
                size,
                tokens.colors.text_primary,
                transform.pre_translate(Vec2::new(bx, by)),
                None,
                TextWeight::Bold,
            );
            if !small.is_empty() {
                let ss = renderer_type_size(text_renderer, tokens.typography.scale.xs);
                let (sw, _) = text_renderer.measure(small, ss, None);
                let sx = center.x - f64::from(sw) / 2.0;
                let sy = center.y + f64::from(bh) / 2.0 - 2.0;
                text_renderer.draw_text(
                    scene,
                    small,
                    ss,
                    tokens.colors.text_dim,
                    transform.pre_translate(Vec2::new(sx, sy)),
                    None,
                );
            }
        }
    }
}

impl Widget for RingGauge {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(self.size),
                height: Dimension::length(self.size),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let radius_box = self.rect.width().min(self.rect.height()) / 2.0;
        let thickness = if self.thickness > 0.0 {
            self.thickness
        } else {
            (radius_box * 0.16).clamp(2.5, 6.0)
        };
        // Inset the arc radius by half the thickness so the round caps stay
        // inside the box.
        let radius = (radius_box - thickness / 2.0).max(0.0);
        let reading = GaugeReading {
            frac: self.frac,
            color: self.resolve_color(theme),
            label: &self.small,
            value: &self.big,
        };
        RingGauge::draw_styled(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            Circle::new(self.rect.center(), radius),
            thickness,
            &reading,
            &self.style,
        );
    }

    fn name(&self) -> &'static str {
        "RingGauge"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        // Expose the 0..1 fraction as a percentage so an AT announces "42
        // percent". The dim caption (`small`) names it, falling back to the
        // prominent (`big`) readout. Both are host display strings, not FTL
        // keys → no `i18n!`.
        let pct = pct(self.frac);
        let name = if !self.small.is_empty() {
            Some(self.small.clone())
        } else if !self.big.is_empty() {
            Some(self.big.clone())
        } else {
            None
        };
        Some(
            AccessNode::new(Role::ProgressBar)
                .with_optional_name(name)
                .with_value(format!("{pct}%")),
        )
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

/// Labelled horizontal bar gauge — a rounded track with a left-anchored
/// fill, a top-left label and a right-aligned value readout.
pub struct BarGauge {
    frac: f32,
    fill: Fillcolor,
    label: String,
    value: String,
    height: f32,
    style: GaugeStyle,
    rect: Rect,
}

impl BarGauge {
    /// `frac` is clamped to `0..1`.
    pub fn new(frac: f32) -> Self {
        Self {
            frac: frac.clamp(0.0, 1.0),
            fill: Fillcolor::Threshold(GaugeThresholds::default()),
            label: String::new(),
            value: String::new(),
            height: 32.0,
            style: GaugeStyle::default(),
            rect: Rect::ZERO,
        }
    }

    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.label = label.into();
        self.value = value.into();
        self
    }

    #[must_use]
    pub fn with_thresholds(mut self, thresholds: GaugeThresholds) -> Self {
        self.fill = Fillcolor::Threshold(thresholds);
        self
    }

    /// Bypasses the threshold bands.
    #[must_use]
    pub fn with_color(mut self, color: Color) -> Self {
        self.fill = Fillcolor::Fixed(color);
        self
    }

    #[must_use]
    pub fn with_style(mut self, style: GaugeStyle) -> Self {
        self.style = style;
        self
    }

    pub fn style(&self) -> &GaugeStyle {
        &self.style
    }

    pub fn style_mut(&mut self) -> &mut GaugeStyle {
        &mut self.style
    }

    #[must_use]
    pub fn with_height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    /// Update the fraction (clamped to `0..1`).
    pub fn set_fraction(&mut self, frac: f32) {
        self.frac = frac.clamp(0.0, 1.0);
    }

    fn resolve_color(&self, theme: &Theme) -> Color {
        match self.fill {
            Fillcolor::Threshold(t) => t.color(theme, self.frac),
            Fillcolor::Fixed(c) => c,
        }
    }

    /// Paint `reading.label` top-left and `reading.value` top-right of
    /// `rect`, over a track along its bottom filled to `reading.frac`.
    pub fn draw_at(ctx: &mut PaintCtx<'_>, rect: Rect, reading: &GaugeReading<'_>) {
        Self::draw_styled(ctx, rect, reading, &GaugeStyle::default());
    }

    /// [`Self::draw_at`] with an explicit [`GaugeStyle`].
    pub fn draw_styled(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        reading: &GaugeReading<'_>,
        style: &GaugeStyle,
    ) {
        if rect.width() <= 0.0 {
            return;
        }
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let GaugeReading {
            frac,
            color,
            label,
            value,
        } = *reading;
        let tokens = &theme.tokens;
        let text_size = renderer_type_size(
            text_renderer,
            if style.compact_labels {
                tokens.typography.scale.xs
            } else {
                tokens.typography.scale.sm
            },
        );
        if !label.is_empty() {
            text_renderer.draw_text_weighted(
                scene,
                label,
                text_size,
                tokens.colors.text_primary,
                transform.pre_translate(Vec2::new(rect.x0, rect.y0)),
                None,
                TextWeight::Medium,
            );
        }
        if !value.is_empty() {
            let (vw, _) = text_renderer.measure(value, text_size, None);
            text_renderer.draw_text(
                scene,
                value,
                text_size,
                tokens.colors.text_dim,
                transform.pre_translate(Vec2::new(rect.x1 - f64::from(vw), rect.y0)),
                None,
            );
        }

        let track = Rect::new(rect.x0, rect.y1 - BAR_TRACK_H, rect.x1, rect.y1);
        let radius = BAR_TRACK_H / 2.0;
        scene.fill(
            Fill::NonZero,
            transform,
            track_color(theme, style.track_alpha),
            None,
            &RoundedRect::from_rect(track, radius),
        );

        let frac = f64::from(frac.clamp(0.0, 1.0));
        if frac > 0.0 {
            let fill = Rect::new(
                track.x0,
                track.y0,
                track.x0 + track.width() * frac,
                track.y1,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &RoundedRect::from_rect(fill, radius),
            );
        }
    }

    /// A label-less span gauge: `track` is the full rounded track and the
    /// fill covers `from..=to` of it (fractions, clamped and ordered) — a
    /// day's low–high on the week's range, an interval on a scale. `fill` is
    /// any brush, so a caller can lay one gradient across the whole track and
    /// let each span show its slice of it. An empty span still paints as a
    /// round dot the track's height wide, so a single value stays visible.
    pub fn draw_span(
        scene: &mut Scene,
        transform: Affine,
        track: Rect,
        theme: &Theme,
        (from, to): (f32, f32),
        fill: &Brush,
        style: &GaugeStyle,
    ) {
        if track.width() <= 0.0 || track.height() <= 0.0 {
            return;
        }
        let radius = track.height() / 2.0;
        scene.fill(
            Fill::NonZero,
            transform,
            track_color(theme, style.track_alpha),
            None,
            &RoundedRect::from_rect(track, radius),
        );
        let (lo, hi) = {
            let (a, b) = (
                f64::from(from.clamp(0.0, 1.0)),
                f64::from(to.clamp(0.0, 1.0)),
            );
            (a.min(b), a.max(b))
        };
        let x0 = track.x0 + track.width() * lo;
        let x1 = (track.x0 + track.width() * hi).max(x0 + track.height());
        scene.fill(
            Fill::NonZero,
            transform,
            fill,
            None,
            &RoundedRect::new(x0, track.y0, x1, track.y1, radius),
        );
    }
}

impl Widget for BarGauge {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(self.height),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let reading = GaugeReading {
            frac: self.frac,
            color: self.resolve_color(theme),
            label: &self.label,
            value: &self.value,
        };
        BarGauge::draw_styled(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
            &reading,
            &self.style,
        );
    }

    fn name(&self) -> &'static str {
        "BarGauge"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        // `label` names it; `value` is the caller's readout when present
        // (e.g. "3.2 GB"), else the fraction as a percentage. Both are
        // display strings, not FTL keys → no `i18n!`.
        let pct = pct(self.frac);
        let value = if self.value.is_empty() {
            format!("{pct}%")
        } else {
            self.value.clone()
        };
        let name = (!self.label.is_empty()).then(|| self.label.clone());
        Some(
            AccessNode::new(Role::ProgressBar)
                .with_optional_name(name)
                .with_value(value),
        )
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello_encoding::DrawTag;

    fn count_color_tags(scene: &Scene) -> usize {
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count()
    }

    #[test]
    fn gauge_ring_and_bar_paint_tracks_labels_and_threshold_bands() {
        /// Threshold colour resolution: below `warning` → success, in the
        /// `[warning, danger)` band → warning, at/above `danger` → danger.
        fn thresholds_pick_semantic_bands() {
            let theme = Theme::dark();
            let t = GaugeThresholds::default(); // warning 0.6, danger 0.85
            let s = &theme.tokens.colors.semantic;
            assert_eq!(t.color(&theme, 0.10), s.success);
            assert_eq!(t.color(&theme, 0.59), s.success);
            assert_eq!(t.color(&theme, 0.60), s.warning, "boundary is inclusive");
            assert_eq!(t.color(&theme, 0.84), s.warning);
            assert_eq!(t.color(&theme, 0.85), s.danger, "boundary is inclusive");
            assert_eq!(t.color(&theme, 1.0), s.danger);
        }

        /// A ring at fraction 0 paints only the track; a non-zero fraction
        /// paints the track *and* the arc (one more stroke).
        fn ring_zero_paints_track_only() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();

            let mut zero = Scene::new();
            RingGauge::draw_at(
                &mut PaintCtx::new(&mut zero, Affine::IDENTITY, &mut tr, &theme),
                Circle::new(Point::new(50.0, 50.0), 40.0),
                6.0,
                &GaugeReading {
                    frac: 0.0,
                    color: theme.accent_primary,
                    label: "",
                    value: "",
                },
            );
            let mut half = Scene::new();
            RingGauge::draw_at(
                &mut PaintCtx::new(&mut half, Affine::IDENTITY, &mut tr, &theme),
                Circle::new(Point::new(50.0, 50.0), 40.0),
                6.0,
                &GaugeReading {
                    frac: 0.5,
                    color: theme.accent_primary,
                    label: "",
                    value: "",
                },
            );
            assert!(
                count_color_tags(&half) > count_color_tags(&zero),
                "non-zero fraction must paint the arc on top of the track",
            );
        }

        /// The ring's centred label paints a glyph run when supplied.
        fn ring_label_paints_glyphs() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            RingGauge::draw_at(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                Circle::new(Point::new(50.0, 50.0), 40.0),
                6.0,
                &GaugeReading {
                    frac: 0.7,
                    color: theme.accent_primary,
                    label: "cpu",
                    value: "70%",
                },
            );
            assert!(
                !scene.encoding().resources.glyph_runs.is_empty(),
                "ring with a label must paint glyph runs",
            );
        }

        /// A ring gauge widget resolves its fill via thresholds: a 0.9 value
        /// is danger, a 0.3 value is success.
        fn ring_widget_uses_thresholds() {
            let theme = Theme::dark();
            let hot = RingGauge::new(0.9);
            assert_eq!(
                hot.resolve_color(&theme),
                theme.tokens.colors.semantic.danger
            );
            let cool = RingGauge::new(0.3);
            assert_eq!(
                cool.resolve_color(&theme),
                theme.tokens.colors.semantic.success
            );
            // Fixed colour overrides thresholds.
            let fixed = RingGauge::new(0.9).with_color(Color::from_rgb8(1, 2, 3));
            assert_eq!(fixed.resolve_color(&theme), Color::from_rgb8(1, 2, 3));
        }

        /// The bar gauge fills proportionally: a 0-fraction bar paints just the
        /// track, a non-zero fraction adds the fill rect.
        fn bar_zero_paints_track_only() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let rect = Rect::new(0.0, 0.0, 200.0, 32.0);

            let mut zero = Scene::new();
            BarGauge::draw_at(
                &mut PaintCtx::new(&mut zero, Affine::IDENTITY, &mut tr, &theme),
                rect,
                &GaugeReading {
                    frac: 0.0,
                    color: theme.accent_primary,
                    label: "",
                    value: "",
                },
            );
            let mut filled = Scene::new();
            BarGauge::draw_at(
                &mut PaintCtx::new(&mut filled, Affine::IDENTITY, &mut tr, &theme),
                rect,
                &GaugeReading {
                    frac: 0.5,
                    color: theme.accent_primary,
                    label: "",
                    value: "",
                },
            );
            assert_eq!(count_color_tags(&zero), 1, "track only at frac 0");
            assert!(
                count_color_tags(&filled) > count_color_tags(&zero),
                "fill adds a rect"
            );
        }

        /// A bar gauge with a label + value paints two glyph runs.
        fn bar_label_and_value_paint() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            BarGauge::draw_at(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                Rect::new(0.0, 0.0, 200.0, 32.0),
                &GaugeReading {
                    frac: 0.4,
                    color: theme.accent_primary,
                    label: "Memory",
                    value: "4.2 GB",
                },
            );
            assert!(
                !scene.encoding().resources.glyph_runs.is_empty(),
                "label/value must paint glyph runs",
            );
        }

        thresholds_pick_semantic_bands();
        ring_zero_paints_track_only();
        ring_label_paints_glyphs();
        ring_widget_uses_thresholds();
        bar_zero_paints_track_only();
        bar_label_and_value_paint();
    }

    /// A span paints its track and its fill even when empty (a dot), and
    /// the style's track alpha reaches the track.
    #[test]
    fn span_paints_track_and_fill() {
        let theme = Theme::dark();
        let mut scene = Scene::new();
        BarGauge::draw_span(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 120.0, 6.0),
            &theme,
            (0.7, 0.7),
            &Brush::Solid(theme.accent_primary),
            &GaugeStyle::default(),
        );
        assert_eq!(count_color_tags(&scene), 2);
        let (faint, firm) = (
            track_color(&theme, TRACK_ALPHA),
            track_color(
                &theme,
                GaugeStyle::default().with_track_alpha(56).track_alpha,
            ),
        );
        assert!(firm.components[3] > faint.components[3]);
    }
}
