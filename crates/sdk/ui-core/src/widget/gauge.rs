//! Gauges — [`RingGauge`] (a circular arc gauge with a centred label) and
//! [`BarGauge`] (a labelled horizontal bar). Both take a `0..1` fraction
//! and pick their fill colour from a set of [`GaugeThresholds`] so a value
//! crossing into the warning / danger band recolours automatically.
//!
//! The ring sweeps clockwise from 12 o'clock (`-π/2`) using a round-capped
//! [`kurbo::Arc`] over a faint full-circle track; the bar fills a rounded
//! track left-to-right. Track, label and threshold colours all come from
//! the [`Theme`] — nothing is hard-coded.

use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, TAU};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Arc, Cap, Circle, Join, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Alpha of a gauge's background track against the border colour.
const TRACK_ALPHA: u8 = 150;
const BAR_TRACK_H: f64 = 8.0;

fn with_alpha(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
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

    #[allow(clippy::too_many_arguments)]
    pub fn draw_at(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        center: Point,
        radius: f64,
        thickness: f64,
        frac: f32,
        color: Color,
        theme: &Theme,
        big: &str,
        small: &str,
    ) {
        if radius <= 0.0 {
            return;
        }
        let track = Circle::new(center, radius);
        scene.stroke(
            &Stroke::new(thickness),
            transform,
            with_alpha(theme.tokens.colors.border_standard, TRACK_ALPHA),
            None,
            &track,
        );

        let frac = f64::from(frac.clamp(0.0, 1.0));
        if frac > 0.0 {
            let arc = Arc::new(center, (radius, radius), -FRAC_PI_2, frac * TAU, 0.0);
            let stroke = Stroke::new(thickness)
                .with_caps(Cap::Round)
                .with_join(Join::Round);
            scene.stroke(&stroke, transform, color, None, &arc);
        }

        let tokens = &theme.tokens;
        if !big.is_empty() {
            let size = tokens.typography.scale.xl;
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
                let ss = tokens.typography.scale.xs;
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
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

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
        let color = self.resolve_color(theme);
        RingGauge::draw_at(
            scene,
            transform,
            text_renderer,
            self.rect.center(),
            radius,
            thickness,
            self.frac,
            color,
            theme,
            &self.big,
            &self.small,
        );
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

    #[allow(clippy::too_many_arguments)]
    pub fn draw_at(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        rect: Rect,
        theme: &Theme,
        frac: f32,
        color: Color,
        label: &str,
        value: &str,
    ) {
        if rect.width() <= 0.0 {
            return;
        }
        let tokens = &theme.tokens;
        if !label.is_empty() {
            text_renderer.draw_text_weighted(
                scene,
                label,
                tokens.typography.scale.sm,
                tokens.colors.text_primary,
                transform.pre_translate(Vec2::new(rect.x0, rect.y0)),
                None,
                TextWeight::Medium,
            );
        }
        if !value.is_empty() {
            let (vw, _) = text_renderer.measure(value, tokens.typography.scale.sm, None);
            text_renderer.draw_text(
                scene,
                value,
                tokens.typography.scale.sm,
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
            with_alpha(tokens.colors.border_standard, TRACK_ALPHA),
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
}

impl Widget for BarGauge {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

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
        let color = self.resolve_color(theme);
        BarGauge::draw_at(
            scene,
            transform,
            text_renderer,
            self.rect,
            theme,
            self.frac,
            color,
            &self.label,
            &self.value,
        );
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

    /// Threshold colour resolution: below `warning` → success, in the
    /// `[warning, danger)` band → warning, at/above `danger` → danger.
    #[test]
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
    #[test]
    fn ring_zero_paints_track_only() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();

        let mut zero = Scene::new();
        RingGauge::draw_at(
            &mut zero,
            Affine::IDENTITY,
            &mut tr,
            Point::new(50.0, 50.0),
            40.0,
            6.0,
            0.0,
            theme.accent_primary,
            &theme,
            "",
            "",
        );
        let mut half = Scene::new();
        RingGauge::draw_at(
            &mut half,
            Affine::IDENTITY,
            &mut tr,
            Point::new(50.0, 50.0),
            40.0,
            6.0,
            0.5,
            theme.accent_primary,
            &theme,
            "",
            "",
        );
        assert!(
            count_color_tags(&half) > count_color_tags(&zero),
            "non-zero fraction must paint the arc on top of the track",
        );
    }

    /// The ring's centred label paints a glyph run when supplied.
    #[test]
    fn ring_label_paints_glyphs() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        RingGauge::draw_at(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            Point::new(50.0, 50.0),
            40.0,
            6.0,
            0.7,
            theme.accent_primary,
            &theme,
            "70%",
            "cpu",
        );
        assert!(
            !scene.encoding().resources.glyph_runs.is_empty(),
            "ring with a label must paint glyph runs",
        );
    }

    /// A ring gauge widget resolves its fill via thresholds: a 0.9 value
    /// is danger, a 0.3 value is success.
    #[test]
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
    #[test]
    fn bar_zero_paints_track_only() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let rect = Rect::new(0.0, 0.0, 200.0, 32.0);

        let mut zero = Scene::new();
        BarGauge::draw_at(
            &mut zero,
            Affine::IDENTITY,
            &mut tr,
            rect,
            &theme,
            0.0,
            theme.accent_primary,
            "",
            "",
        );
        let mut filled = Scene::new();
        BarGauge::draw_at(
            &mut filled,
            Affine::IDENTITY,
            &mut tr,
            rect,
            &theme,
            0.5,
            theme.accent_primary,
            "",
            "",
        );
        assert_eq!(count_color_tags(&zero), 1, "track only at frac 0");
        assert!(
            count_color_tags(&filled) > count_color_tags(&zero),
            "fill adds a rect"
        );
    }

    /// A bar gauge with a label + value paints two glyph runs.
    #[test]
    fn bar_label_and_value_paint() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        BarGauge::draw_at(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            Rect::new(0.0, 0.0, 200.0, 32.0),
            &theme,
            0.4,
            theme.accent_primary,
            "Memory",
            "4.2 GB",
        );
        assert!(
            !scene.encoding().resources.glyph_runs.is_empty(),
            "label/value must paint glyph runs",
        );
    }
}
