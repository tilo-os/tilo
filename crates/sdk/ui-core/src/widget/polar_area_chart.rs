//! Interactive polar-area (rose / radial bar) chart.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "bounded polar geometry uses floating point"
)]

use std::f64::consts::{FRAC_PI_2, TAU};
use tilo_sort::CompactSort as _;
use vello::Scene;
use vello::kurbo::{Affine, Arc, Circle, ParamCurve, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

use crate::paint::with_alpha;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::chart_data::ChartItem;
use crate::widget::chart_labels::format_compact;
use crate::widget::chart_navigation::draw_title;
use crate::widget::plot::{Plot, PlotFrame, PlotItem, PlotMarks};

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

pub struct PolarAreaMarks {
    data: Vec<ChartItem>,
    style: PolarAreaStyle,
    center_title: String,
    center_subtitle: String,
    frame: PlotFrame<String>,
    sectors: Vec<Sector>,
}

/// Equal-angle sectors whose radius shows each value (a Nightingale rose).
pub type PolarAreaChart = Plot<PolarAreaMarks>;

impl PolarAreaChart {
    pub fn new(data: Vec<ChartItem>) -> Self {
        Plot::from_marks(PolarAreaMarks {
            data,
            style: PolarAreaStyle::default(),
            center_title: String::new(),
            center_subtitle: String::new(),
            frame: PlotFrame::new(320.0, 140.0),
            sectors: Vec::new(),
        })
    }
    #[must_use]
    pub fn with_style(mut self, style: PolarAreaStyle) -> Self {
        self.marks.style = style;
        self
    }
    #[must_use]
    pub fn with_center(mut self, title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        self.marks.center_title = title.into();
        self.marks.center_subtitle = subtitle.into();
        self
    }
    pub fn set_data(&mut self, data: Vec<ChartItem>) {
        self.marks.data = data;
        self.marks.frame.inspection.hovered = None;
    }
    pub fn inspect(&mut self, id: &str) -> bool {
        let shown = self
            .marks
            .data
            .iter()
            .any(|datum| datum.id == id && datum.value.is_finite() && datum.value >= 0.0);
        self.inspect_item(shown.then(|| id.to_owned()))
    }
}

impl PolarAreaMarks {
    fn geometry(&self) -> (Point, f64, f64) {
        let top = if self.frame.title.is_empty() {
            12.0
        } else {
            40.0
        };
        let area = Rect::new(
            self.frame.rect.x0 + 14.0,
            self.frame.rect.y0 + top,
            self.frame.rect.x1 - 14.0,
            self.frame.rect.y1 - 26.0,
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
        let mut placer = crate::widget::chart_labels::LabelPlacer::new(self.frame.rect, theme);
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
        order.compact_sort_by(|a, b| b.3.total_cmp(&a.3));
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
    fn datum(&self, id: &str) -> Option<&ChartItem> {
        self.data.iter().find(|d| d.id == id)
    }
    fn color(datum: &ChartItem, index: usize, theme: &Theme) -> Color {
        datum
            .color
            .unwrap_or_else(|| theme.tokens.colors.presence.series(index))
    }
    fn hit_mark(&self, point: Point) -> Option<&str> {
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

impl PlotMarks for PolarAreaMarks {
    type Hover = String;
    type Activated = String;
    const NAME: &'static str = "PolarAreaChart";
    const LABEL: &'static str = "Polar area chart";

    fn frame(&self) -> &PlotFrame<String> {
        &self.frame
    }
    fn frame_mut(&mut self) -> &mut PlotFrame<String> {
        &mut self.frame
    }
    fn hit(&self, point: Point) -> Option<String> {
        self.hit_mark(point).map(str::to_owned)
    }
    fn activation(&self, id: &String) -> Option<String> {
        Some(id.clone())
    }
    fn keyboard_items(&self) -> Vec<String> {
        let shown = self
            .data
            .iter()
            .filter(|datum| datum.value.is_finite() && datum.value >= 0.0);
        shown.map(|datum| datum.id.clone()).collect()
    }
    fn anchor(&self, id: &String) -> Option<Point> {
        let sector = self.sectors.iter().find(|sector| sector.id == *id)?;
        let (center, _, _) = self.geometry();
        let angle = sector.start + sector.sweep / 2.0;
        let radius = sector.inner.midpoint(sector.outer);
        Some(Point::new(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        ))
    }
    fn items(&self) -> Vec<PlotItem<String>> {
        self.data
            .iter()
            .filter(|d| d.value.is_finite() && d.value >= 0.0)
            .map(|d| {
                PlotItem::new(
                    &d.id,
                    &d.label,
                    if d.detail.is_empty() {
                        format_compact(d.value)
                    } else {
                        d.detail.clone()
                    },
                    d.id.clone(),
                )
            })
            .collect()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "rings, sectors, radial labels, center copy, and inspector share polar geometry"
    )]
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        draw_title(
            scene,
            transform,
            tr,
            theme,
            self.frame.rect,
            &self.frame.title,
        );
        let (center, inner, outer) = self.geometry();
        let max = self.max();
        let rings = self.style.rings.max(1);
        for ring in 1..=rings {
            let fraction = f64::from(ring) / f64::from(rings);
            let radius = fraction.mul_add(outer - inner, inner);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                with_alpha(theme.tokens.colors.text_dim, 45),
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
            let hovered = self.frame.inspection.hovered.as_deref() == Some(&datum.id);
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
                    format!("{}  {}", datum.label, format_compact(datum.value))
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
        self.follow_keyboard_anchor();
        if let Some(datum) = self
            .frame
            .inspection
            .hovered
            .as_deref()
            .and_then(|id| self.datum(id))
            .filter(|_| self.style.tooltip)
        {
            let detail = if datum.detail.is_empty() {
                format_compact(datum.value)
            } else {
                format!("{} · {}", format_compact(datum.value), datum.detail)
            };
            crate::widget::chart_labels::draw_tooltip(
                scene,
                transform,
                tr,
                theme,
                self.frame.inspection.pointer,
                self.frame.rect,
                140.0,
                (&datum.label, &detail),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_max_requires_positive_finite_value() {
        let mut chart = PolarAreaChart::new(vec![ChartItem::new("a", "A", 7.0)]);
        chart.marks.style.max = Some(-2.0);
        assert_eq!(chart.marks.max(), 7.0);
        chart.marks.style.max = Some(10.0);
        assert_eq!(chart.marks.max(), 10.0);
    }
}
