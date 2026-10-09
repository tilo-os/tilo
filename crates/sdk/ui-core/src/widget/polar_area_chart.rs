//! Interactive polar-area (rose / radial bar) chart.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "bounded polar geometry uses floating point"
)]

use crate::widget::ModelPath;
use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, TAU};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Arc, Circle, ParamCurve, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use crate::a11y::{AccessNode, AccessState, Role};
use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::chart_navigation::{
    ChartNavAction, ChartTouchTracker, InteractiveChart, handle_chart_event, moved_index,
};
use crate::widget::{TaffyTreeExt, Widget};

fn alpha(color: Color, value: u8) -> Color {
    Color::from_rgba8(color.r(), color.g(), color.b(), value)
}

#[derive(Clone, Debug)]
pub struct PolarAreaDatum {
    pub id: String,
    pub label: String,
    pub value: f64,
    pub color: Option<Color>,
    pub detail: String,
}

impl PolarAreaDatum {
    pub fn new(id: impl Into<String>, label: impl Into<String>, value: f64) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value,
            color: None,
            detail: String::new(),
        }
    }
    #[must_use]
    pub fn colored(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent polar presentation toggles are composable"
)]
#[derive(Clone, Debug)]
pub struct PolarAreaStyle {
    pub max: Option<f64>,
    pub start_angle: f64,
    pub inner_radius: f64,
    pub angular_gap: f64,
    pub rings: u8,
    pub radius_exponent: f64,
    pub hover_offset: f64,
    pub show_labels: bool,
    pub show_values: bool,
    pub tooltip: bool,
}

impl Default for PolarAreaStyle {
    fn default() -> Self {
        Self {
            max: None,
            start_angle: -FRAC_PI_2,
            inner_radius: 0.16,
            angular_gap: 0.025,
            rings: 4,
            radius_exponent: 0.5,
            hover_offset: 6.0,
            show_labels: true,
            show_values: false,
            tooltip: true,
        }
    }
}

#[derive(Clone, Debug)]
struct Sector {
    id: String,
    start: f64,
    sweep: f64,
    inner: f64,
    outer: f64,
}

pub struct PolarAreaChart {
    data: Vec<PolarAreaDatum>,
    style: PolarAreaStyle,
    title: String,
    center_title: String,
    center_subtitle: String,
    height: f32,
    rect: Rect,
    pointer: Point,
    sectors: Vec<Sector>,
    hovered: Option<String>,
    activated: Option<String>,
    keyboard_inspection: bool,
    touch: ChartTouchTracker,
}

impl PolarAreaChart {
    pub fn new(data: Vec<PolarAreaDatum>) -> Self {
        Self {
            data,
            style: PolarAreaStyle::default(),
            title: String::new(),
            center_title: String::new(),
            center_subtitle: String::new(),
            height: 320.0,
            rect: Rect::ZERO,
            pointer: Point::ZERO,
            sectors: Vec::new(),
            hovered: None,
            activated: None,
            keyboard_inspection: false,
            touch: ChartTouchTracker::default(),
        }
    }
    #[must_use]
    pub fn styled(mut self, style: PolarAreaStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn with_style(self, style: PolarAreaStyle) -> Self {
        self.styled(style)
    }
    pub fn style(&self) -> &PolarAreaStyle {
        &self.style
    }
    pub fn style_mut(&mut self) -> &mut PolarAreaStyle {
        &mut self.style
    }
    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }
    #[must_use]
    pub fn with_center(mut self, title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        self.center_title = title.into();
        self.center_subtitle = subtitle.into();
        self
    }
    #[must_use]
    pub fn with_height(mut self, height: f32) -> Self {
        self.height = height.max(140.0);
        self
    }
    pub fn set_data(&mut self, data: Vec<PolarAreaDatum>) {
        self.data = data;
        self.hovered = None;
    }
    pub fn take_activated(&mut self) -> Option<String> {
        self.activated.take()
    }
    pub fn inspect(&mut self, id: &str) -> bool {
        self.hovered = self
            .data
            .iter()
            .any(|datum| datum.id == id && datum.value.is_finite() && datum.value >= 0.0)
            .then(|| id.to_owned());
        self.keyboard_inspection = self.hovered.is_some();
        self.update_inspection_anchor();
        self.hovered.is_some()
    }
    pub fn clear_inspection(&mut self) {
        self.hovered = None;
        self.keyboard_inspection = false;
    }

    fn update_inspection_anchor(&mut self) {
        let Some(id) = self.hovered.as_deref() else {
            return;
        };
        let Some(sector) = self.sectors.iter().find(|sector| sector.id == id) else {
            return;
        };
        let (center, _, _) = self.geometry();
        let angle = sector.start + sector.sweep / 2.0;
        let radius = sector.inner.midpoint(sector.outer);
        self.pointer = Point::new(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        );
    }

    fn geometry(&self) -> (Point, f64, f64) {
        let top = if self.title.is_empty() { 12.0 } else { 40.0 };
        let area = Rect::new(
            self.rect.x0 + 14.0,
            self.rect.y0 + top,
            self.rect.x1 - 14.0,
            self.rect.y1 - 26.0,
        );
        let radius = (area.width().min(area.height()) / 2.0 - 18.0).max(1.0);
        (
            area.center(),
            radius * self.style.inner_radius.clamp(0.0, 0.92),
            radius,
        )
    }
    /// Each sector's label inside its sector, centred on the sector's
    /// middle, largest sector first: shortened to the room the sector has
    /// there and dropped (the tooltip names it) where it would spill out of
    /// the sector or collide with a label already placed.
    fn draw_sector_labels(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        labels: &[(String, f64, f64, f64)],
    ) {
        let (center, inner, _) = self.geometry();
        let style = theme.text(TextRole::Caption);
        let mut placer = crate::widget::chart_labels::LabelPlacer::new(self.rect, theme);
        if let Some(((_, title), subtitle)) = crate::widget::chart_labels::centre_label_layout(
            tr,
            theme,
            center,
            inner,
            &self.center_title,
            &self.center_subtitle,
        ) {
            placer.block(subtitle.map_or(title, |sub| title.union(sub)));
        }
        let mut order: Vec<&(String, f64, f64, f64)> = labels.iter().collect();
        order.sort_by(|a, b| b.3.total_cmp(&a.3));
        for (label, start, sweep, radius) in order {
            let angle = start + sweep / 2.0;
            let mid_r = inner.midpoint(*radius);
            let anchor = Point::new(
                center.x + mid_r * angle.cos(),
                center.y + mid_r * angle.sin(),
            );
            // The chord of the sector through its middle bounds the width.
            let room = (2.0 * mid_r * (sweep / 2.0).sin()).min(radius - inner);
            let Some((shown, w, h)) =
                crate::widget::chart_labels::fit_label(tr, label, room, &style)
            else {
                continue;
            };
            let rect = Rect::from_center_size(anchor, (w, h));
            let inside = [
                Point::new(rect.x0, rect.y0),
                Point::new(rect.x1, rect.y0),
                Point::new(rect.x0, rect.y1),
                Point::new(rect.x1, rect.y1),
            ]
            .iter()
            .all(|corner| {
                let dx = corner.x - center.x;
                let dy = corner.y - center.y;
                let r = dx.hypot(dy);
                let a = (dy.atan2(dx) - start).rem_euclid(TAU);
                r >= inner && r <= *radius && a <= *sweep
            });
            if !inside || !placer.place(rect) {
                continue;
            }
            tr.draw_style(
                scene,
                &shown,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((rect.x0, rect.y0)),
                None,
                &style,
            );
        }
    }

    fn max(&self) -> f64 {
        self.style
            .max
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or_else(|| {
                self.data
                    .iter()
                    .map(|d| d.value)
                    .filter(|v| v.is_finite())
                    .fold(0.0_f64, f64::max)
                    .max(1.0)
            })
    }
    fn datum(&self, id: &str) -> Option<&PolarAreaDatum> {
        self.data.iter().find(|d| d.id == id)
    }
    fn color(datum: &PolarAreaDatum, index: usize, theme: &Theme) -> Color {
        datum
            .color
            .unwrap_or_else(|| theme.tokens.colors.presence.series(index))
    }
    fn hit(&self, point: Point) -> Option<&str> {
        let (center, _, _) = self.geometry();
        let dx = point.x - center.x;
        let dy = point.y - center.y;
        let radius = dx.hypot(dy);
        let angle =
            (dy.atan2(dx) - self.style.start_angle).rem_euclid(TAU) + self.style.start_angle;
        self.sectors
            .iter()
            .find(|s| {
                angle >= s.start
                    && angle <= s.start + s.sweep
                    && radius >= s.inner
                    && radius <= s.outer + self.style.hover_offset
            })
            .map(|s| s.id.as_str())
    }
}

fn sector_path(
    center: Point,
    inner: f64,
    outer: f64,
    start: f64,
    sweep: f64,
) -> vello::kurbo::BezPath {
    let mut path = vello::kurbo::BezPath::new();
    let outer_arc = Arc::new(center, (outer, outer), start, sweep, 0.0);
    let inner_arc = Arc::new(center, (inner, inner), start + sweep, -sweep, 0.0);
    path.move_to(outer_arc.eval(0.0));
    outer_arc.to_cubic_beziers(0.1, |p1, p2, p3| path.curve_to(p1, p2, p3));
    path.line_to(inner_arc.eval(0.0));
    inner_arc.to_cubic_beziers(0.1, |p1, p2, p3| path.curve_to(p1, p2, p3));
    path.close_path();
    path
}

impl InteractiveChart for PolarAreaChart {
    fn interaction(&mut self) -> (&mut Point, &mut bool, &mut ChartTouchTracker) {
        (
            &mut self.pointer,
            &mut self.keyboard_inspection,
            &mut self.touch,
        )
    }
    fn update_hover(&mut self) {
        self.hovered = self.hit(self.pointer).map(str::to_owned);
    }
    fn clear_hover(&mut self) {
        self.hovered = None;
    }
    fn activate_hovered(&mut self) -> bool {
        let Some(id) = &self.hovered else {
            return false;
        };
        self.activated = Some(id.clone());
        true
    }
    fn move_keyboard_inspection(&mut self, action: ChartNavAction) -> bool {
        let candidates = self
            .data
            .iter()
            .filter(|datum| datum.value.is_finite() && datum.value >= 0.0)
            .map(|datum| datum.id.as_str())
            .collect::<Vec<_>>();
        let current = self
            .hovered
            .as_deref()
            .and_then(|id| candidates.iter().position(|candidate| *candidate == id));
        self.hovered = moved_index(current, candidates.len(), action)
            .and_then(|index| candidates.get(index))
            .map(|id| (*id).to_owned());
        self.update_inspection_anchor();
        self.hovered.is_some()
    }
}

impl Widget for PolarAreaChart {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(self.height),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }
    #[allow(
        clippy::too_many_lines,
        reason = "rings, sectors, radial labels, center copy, and inspector share polar geometry"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if !self.title.is_empty() {
            tr.draw_style(
                scene,
                &self.title,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((self.rect.x0 + 12.0, self.rect.y0 + 8.0)),
                None,
                &theme.text(TextRole::Heading),
            );
        }
        let (center, inner, outer) = self.geometry();
        let max = self.max();
        let rings = self.style.rings.max(1);
        for ring in 1..=rings {
            let fraction = f64::from(ring) / f64::from(rings);
            let radius = fraction.mul_add(outer - inner, inner);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                alpha(theme.tokens.colors.text_dim, 45),
                None,
                &Circle::new(center, radius),
            );
        }
        let valid = self
            .data
            .iter()
            .filter(|d| d.value.is_finite() && d.value >= 0.0)
            .count();
        self.sectors.clear();
        if valid == 0 {
            return;
        }
        let count = f64::from(u32::try_from(valid).unwrap_or(u32::MAX));
        let slot = TAU / count;
        let gap = self.style.angular_gap.clamp(0.0, slot * 0.7);
        let exponent = self.style.radius_exponent.max(0.01);
        let mut labels: Vec<(String, f64, f64, f64)> = Vec::new();
        for (index, datum) in self
            .data
            .iter()
            .filter(|d| d.value.is_finite() && d.value >= 0.0)
            .enumerate()
        {
            let start = self.style.start_angle
                + f64::from(u32::try_from(index).unwrap_or(u32::MAX)) * slot
                + gap / 2.0;
            let sweep = slot - gap;
            let fraction = (datum.value / max).clamp(0.0, 1.0).powf(exponent);
            let mut radius = fraction.mul_add(outer - inner, inner);
            let hovered = self.hovered.as_deref() == Some(&datum.id);
            if hovered {
                radius += self.style.hover_offset;
            }
            let color = Self::color(datum, index, theme);
            scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &sector_path(center, inner, radius, start, sweep),
            );
            self.sectors.push(Sector {
                id: datum.id.clone(),
                start,
                sweep,
                inner,
                outer: radius,
            });
            if self.style.show_labels {
                let label = if self.style.show_values {
                    format!("{}  {}", datum.label, format_value(datum.value))
                } else {
                    datum.label.clone()
                };
                labels.push((label, start, sweep, radius));
            }
        }
        self.draw_sector_labels(scene, transform, tr, theme, &labels);
        // The centre copy at the largest size the hole holds; left out
        // where even a heading does not fit.
        let _ = crate::widget::chart_labels::paint_centre_label(
            scene,
            transform,
            tr,
            theme,
            center,
            inner,
            &self.center_title,
            &self.center_subtitle,
        );
        if self.keyboard_inspection {
            self.update_inspection_anchor();
        }
        if let Some(datum) = self
            .hovered
            .as_deref()
            .and_then(|id| self.datum(id))
            .filter(|_| self.style.tooltip)
        {
            let detail = if datum.detail.is_empty() {
                format_value(datum.value)
            } else {
                format!("{} · {}", format_value(datum.value), datum.detail)
            };
            let (label_width, _) =
                tr.measure_style(&datum.label, None, &theme.text(TextRole::Caption));
            let (detail_width, _) = tr.measure_style(&detail, None, &theme.text(TextRole::Caption));
            let width = f64::from(label_width.max(detail_width)).max(140.0) + 28.0;
            let x = (self.pointer.x + 16.0)
                .min(self.rect.x1 - width - 8.0)
                .max(self.rect.x0 + 8.0);
            let y = (self.pointer.y + 16.0)
                .min(self.rect.y1 - 72.0)
                .max(self.rect.y0 + 8.0);
            let tip = Rect::new(x, y, x + width, y + 64.0);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.overlay,
                None,
                &vello::kurbo::RoundedRect::from_rect(tip, f64::from(theme.tokens.radii.control)),
            );
            tr.draw_style(
                scene,
                &datum.label,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((x + 14.0, y + 11.0)),
                None,
                &theme
                    .text(TextRole::Label)
                    .with_weight(TextWeight::Semibold),
            );
            tr.draw_style(
                scene,
                &detail,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((x + 14.0, y + 37.0)),
                None,
                &theme.text(TextRole::Caption),
            );
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        handle_chart_event(self, event, ctx);
    }
    fn accessibility(&self) -> Option<AccessNode> {
        let mut root = AccessNode::new(Role::Group).with_name(if self.title.is_empty() {
            "Polar area chart"
        } else {
            &self.title
        });
        root.children = self
            .data
            .iter()
            .filter(|d| d.value.is_finite() && d.value >= 0.0)
            .map(|d| {
                AccessNode::new(Role::Button)
                    .with_action_target(&d.id)
                    .with_name(&d.label)
                    .with_description(if d.detail.is_empty() {
                        format_value(d.value)
                    } else {
                        d.detail.clone()
                    })
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    })
            })
            .collect();
        Some(root)
    }
    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        keys: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let Some(key) = keys.last() else {
            return false;
        };
        if let Some(datum) = self
            .data
            .iter()
            .find(|d| crate::a11y::hash_key(&d.id) == *key)
        {
            self.activated = Some(datum.id.clone());
            true
        } else {
            false
        }
    }
    fn name(&self) -> &'static str {
        "PolarAreaChart"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

fn format_value(value: f64) -> String {
    if value.abs() >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value.abs() >= 1_000.0 {
        format!("{:.1}K", value / 1_000.0)
    } else if value.fract().abs() < 0.001 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_max_requires_positive_finite_value() {
        let mut chart = PolarAreaChart::new(vec![PolarAreaDatum::new("a", "A", 7.0)]);
        chart.style.max = Some(-2.0);
        assert_eq!(chart.max(), 7.0);
        chart.style.max = Some(10.0);
        assert_eq!(chart.max(), 10.0);
    }
}
