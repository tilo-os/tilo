//! Interactive polar-area (rose / radial bar) chart.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "bounded polar geometry uses floating point"
)]

use std::f64::consts::{FRAC_PI_2, TAU};
use tilo_sort::CompactSort as _;
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

use crate::paint::with_alpha;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::chart_data::ChartItem;
use crate::widget::chart_geometry::annulus;
use crate::widget::chart_labels::format_compact;
use crate::widget::chart_navigation::draw_title;
use crate::widget::chart_readout::Readout;
use crate::widget::plot::{Margins, Plot, PlotFrame, PlotItem, PlotMarks};

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
    /// Its place in [`PolarAreaMarks::data`].
    index: usize,
    /// Its place among the sectors drawn, which its colour follows.
    slot: usize,
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
    /// Each sector's whole label, where it fits: inside its sector, or outside it with a
    /// leader line to the sector. Its datum's place in `data`, its colour's slot, the rect the
    /// words take and the leader's two ends where it stands outside.
    labels: Vec<(usize, usize, Rect, Option<[Point; 2]>)>,
    /// The legend naming every sector, where at least one label has no whole room inside its
    /// sector: its datum's place in `data`, its colour's slot and the entry's rect.
    legend: Vec<(usize, usize, Rect)>,
}

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
            labels: Vec::new(),
            legend: Vec::new(),
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
        // A hovered sector reaches past the others by its offset, so the ring keeps that margin.
        let area = self.frame.inside();
        let radius = (area.width().min(area.height()) / 2.0 - self.style.hover_offset).max(1.0);
        (
            area.center(),
            radius * self.style.inner_radius.clamp(0.0, 0.92),
            radius,
        )
    }
    /// Each sector round the chart, in the order it takes: every direction the same angle,
    /// its length carrying the value.
    fn sector_layout(&self, inner: f64, outer: f64) -> Vec<Sector> {
        let max = self.max();
        let valid: Vec<(usize, &ChartItem)> = self
            .data
            .iter()
            .enumerate()
            .filter(|(_, d)| d.value.is_finite() && d.value >= 0.0)
            .collect();
        let count = f64::from(u32::try_from(valid.len()).unwrap_or(u32::MAX));
        if count <= 0.0 {
            return Vec::new();
        }
        let slot = TAU / count;
        let gap = self.style.angular_gap.clamp(0.0, slot * 0.7);
        let exponent = self.style.radius_exponent.max(0.01);
        valid
            .into_iter()
            .enumerate()
            .map(|(slot_index, (index, datum))| {
                let start = self.style.start_angle
                    + f64::from(u32::try_from(slot_index).unwrap_or(u32::MAX)) * slot
                    + gap / 2.0;
                let sweep = slot - gap;
                let fraction = (datum.value / max).clamp(0.0, 1.0).powf(exponent);
                Sector {
                    index,
                    slot: slot_index,
                    id: datum.id.clone(),
                    start,
                    sweep,
                    inner,
                    outer: fraction.mul_add(outer - inner, inner),
                }
            })
            .collect()
    }

    /// The words a sector shows: its name, and its value where the chart shows values.
    fn label_text(&self, index: usize) -> String {
        let Some(datum) = self.data.get(index) else {
            return String::new();
        };
        if self.style.show_values {
            format!("{}  {}", datum.label, format_compact(datum.value))
        } else {
            datum.label.clone()
        }
    }

    /// Each sector's label whole, largest sector first, centred in its sector where it fits
    /// there and standing outside it with a leader line where it does not: a label is never cut
    /// to a stub, since a shared prefix names nothing. Where even the room beside the circle
    /// fails, the chart names every sector in a legend under it instead.
    fn place_sector_labels(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        self.labels.clear();
        self.legend.clear();
        if !self.style.show_labels {
            return;
        }
        let (center, inner, outer) = self.geometry();
        let sectors = self.sector_layout(inner, outer);
        if sectors.is_empty() {
            return;
        }
        let style = theme.text(TextRole::Caption);
        let mut placer = crate::widget::chart_labels::LabelPlacer::new(self.frame.inside(), theme);
        if let Some(label) = crate::widget::chart_labels::centre_label_layout(
            tr,
            theme,
            center,
            inner,
            &self.center_title,
            &self.center_subtitle,
        ) {
            placer.block(
                label
                    .subtitle
                    .map_or(label.rect, |sub| label.rect.union(sub)),
            );
        }
        let mut order: Vec<&Sector> = sectors
            .iter()
            .filter(|sector| !self.label_text(sector.index).is_empty())
            .collect();
        if order.is_empty() {
            return;
        }
        order.compact_sort_by(|a, b| b.outer.total_cmp(&a.outer));
        let mut labels = Vec::new();
        for sector in &order {
            let text = self.label_text(sector.index);
            let angle = sector.start + sector.sweep / 2.0;
            let mid = inner.midpoint(sector.outer);
            let anchor = Point::new(center.x + mid * angle.cos(), center.y + mid * angle.sin());
            let (w, h) = tr.measure_style(&text, None, &style);
            let (w, h) = (f64::from(w), f64::from(h));
            let inside = Rect::from_center_size(anchor, (w, h));
            if sector_holds(inside, center, sector) && placer.place(inside) {
                labels.push((sector.index, sector.slot, inside, None));
                continue;
            }
            // Not whole inside: it stands outside the sector with a leader line to it, in
            // the room the chart leaves beside the circle.
            let edge = sector.outer + self.style.hover_offset + f64::from(theme.tokens.spacing.s1);
            let outside =
                crate::widget::chart_labels::radial_label_rect(center, edge, angle, (w, h));
            if !placer.place(outside) {
                continue;
            }
            let ring = Point::new(
                center.x + sector.outer * angle.cos(),
                center.y + sector.outer * angle.sin(),
            );
            let at = Point::new(
                ring.x.clamp(outside.x0, outside.x1),
                ring.y.clamp(outside.y0, outside.y1),
            );
            labels.push((sector.index, sector.slot, outside, Some([ring, at])));
        }
        if labels.len() == order.len() {
            self.labels = labels;
        } else {
            self.place_legend(tr, theme, &order);
        }
    }

    /// A legend naming every sector, largest first, wrapped under the chart: the circle gives
    /// up as much height as its rows take, so no row covers it.
    fn place_legend(&mut self, tr: &mut TextRenderer, theme: &Theme, order: &[&Sector]) {
        let labels: Vec<String> = order
            .iter()
            .map(|sector| self.label_text(sector.index))
            .collect();
        let entries =
            crate::widget::chart_legend::measure(tr, theme, labels.iter().map(String::as_str));
        let rows = crate::widget::chart_legend::wrap(
            &entries,
            self.frame.rect.x0,
            self.frame.rect.x1,
            0.0,
            theme,
        );
        let height = rows.iter().map(|row| row.y1).fold(0.0, f64::max);
        self.frame.margins.bottom = height + f64::from(theme.tokens.spacing.rhythm.section);
        let top = self.frame.rect.y1 - height;
        self.legend = order
            .iter()
            .zip(rows)
            .map(|(sector, row)| {
                (
                    sector.index,
                    sector.slot,
                    row + vello::kurbo::Vec2::new(0.0, top),
                )
            })
            .collect();
    }

    /// The ink a label reads in: the sector's own ink where it stands inside the sector, and
    /// the page's where it stands beside the circle, since that is what it is drawn on.
    fn label_ink(&self, index: usize, slot: usize, outside: bool, theme: &Theme) -> Color {
        if outside {
            return theme.tokens.colors.text_primary;
        }
        let fill = self.data.get(index).map_or_else(
            || theme.tokens.colors.text_primary,
            |datum| Self::color(datum, slot, theme),
        );
        // Dark words on a dark sector would not be read: the ink keeps its contrast.
        theme.tokens.colors.ink_on(
            fill,
            theme.tokens.colors.text_primary,
            crate::tokens::colors::ON_FILL_MIN_CONTRAST,
        )
    }

    fn draw_labels(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let style = theme.text(TextRole::Caption);
        for (index, slot, rect, leader) in &self.labels {
            let text = self.label_text(*index);
            if text.is_empty() {
                continue;
            }
            if let Some([from, to]) = leader {
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    theme.tokens.colors.border_standard,
                    None,
                    &vello::kurbo::Line::new(*from, *to),
                );
            }
            tr.draw_style(
                scene,
                &text,
                self.label_ink(*index, *slot, leader.is_some(), theme),
                transform * Affine::translate((rect.x0, rect.y0)),
                None,
                &style,
            );
        }
    }

    fn draw_legend(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        for (index, slot, rect) in &self.legend {
            let Some(datum) = self.data.get(*index) else {
                continue;
            };
            crate::widget::chart_legend::paint_entry_emphasised(
                scene,
                transform,
                tr,
                theme,
                *rect,
                Self::color(datum, *slot, theme),
                &self.label_text(*index),
                self.frame.inspection.hovered.as_deref() == Some(&datum.id),
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

    fn place(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        self.frame.margins = Margins::header(theme, &self.frame.title, 0);
        self.place_sector_labels(tr, theme);
    }

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
        self.sectors = self.sector_layout(inner, outer);
        if self.sectors.is_empty() {
            return;
        }
        for sector in &self.sectors {
            let Some(datum) = self.data.get(sector.index) else {
                continue;
            };
            let mut radius = sector.outer;
            if self.frame.inspection.hovered.as_deref() == Some(&datum.id) {
                radius += self.style.hover_offset;
            }
            let color = Self::color(datum, sector.slot, theme);
            scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &annulus(
                    center,
                    inner,
                    radius,
                    sector.start,
                    sector.start + sector.sweep,
                ),
            );
        }
        // Labels placed by the layout, or the legend that names every sector where one had no
        // whole room: never a label cut to a stub that names nothing.
        if self.legend.is_empty() {
            self.draw_labels(scene, transform, tr, theme);
        } else {
            self.draw_legend(scene, transform, tr, theme);
        }
        // The centre copy at the largest size the hole holds; left out
        // where even a short line does not fit.
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
    }

    fn readout(&self, _theme: &Theme) -> Option<Readout> {
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
            return Some(Readout::of_mark(
                &datum.label,
                &detail,
                self.frame.inspection.pointer,
                self.frame.rect,
            ));
        }
        None
    }
}

mod polar_area_chart_component;
pub use polar_area_chart_component::{PolarAreaChartProps, PolarAreaValue, areas_description};

/// Whether `rect` lies wholly inside `sector`: every corner within its annulus and its
/// angular span.
fn sector_holds(rect: Rect, center: Point, sector: &Sector) -> bool {
    [
        Point::new(rect.x0, rect.y0),
        Point::new(rect.x1, rect.y0),
        Point::new(rect.x0, rect.y1),
        Point::new(rect.x1, rect.y1),
    ]
    .iter()
    .all(|corner| {
        let dx = corner.x - center.x;
        let dy = corner.y - center.y;
        let radius = dx.hypot(dy);
        let angle = (dy.atan2(dx) - sector.start).rem_euclid(TAU);
        radius >= sector.inner && radius <= sector.outer && angle <= sector.sweep
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn months() -> Vec<ChartItem> {
        [
            ("Jan", 120.0),
            ("Feb", 98.0),
            ("Mar", 145.0),
            ("Apr", 162.0),
            ("May", 138.0),
            ("Jun", 174.0),
            ("Jul", 151.0),
            ("Aug", 119.0),
            ("Sep", 203.0),
            ("Oct", 221.0),
            ("Nov", 188.0),
            ("Dec", 146.0),
        ]
        .into_iter()
        .map(|(label, value)| ChartItem::new(label, label, value))
        .collect()
    }

    fn weeks() -> Vec<ChartItem> {
        [
            ("The week after the launch", 214.0),
            ("The week of the conference", 176.0),
            ("The week the pricing changed for every plan", 132.0),
            ("The week of the outage", 61.0),
            ("The week before the holidays", 149.0),
            ("The week after the newsletter", 195.0),
        ]
        .into_iter()
        .map(|(label, value)| ChartItem::new(label, label, value))
        .collect()
    }

    /// Places `data` in a `width`-wide, 320-tall chart, as the layout does.
    fn placed(data: Vec<ChartItem>, width: f64, theme: &Theme) -> PolarAreaChart {
        let mut chart = PolarAreaChart::new(data);
        chart.marks.frame.rect = Rect::new(0.0, 0.0, width, 320.0);
        crate::text_render::with_layout_renderer(theme, |tr| chart.marks.place(tr, theme));
        chart
    }

    /// Every sector is told apart: all named by a whole label inside their sector, or all
    /// named by the legend, never by a stub cut to the words every label shares.
    #[test]
    fn every_sector_is_named_by_a_whole_label_or_the_legend() {
        let theme = Theme::light();
        for (data, width) in [
            (months(), 1000.0),
            (months(), 358.0),
            (weeks(), 1000.0),
            (weeks(), 358.0),
        ] {
            let count = data.len();
            let chart = placed(data, width, &theme);
            let labels = &chart.marks.labels;
            let legend = &chart.marks.legend;
            assert!(
                labels.len() == count || legend.len() == count,
                "at {width}: {} labels and {} legend entries for {count} sectors",
                labels.len(),
                legend.len()
            );
            let mut named: Vec<String> = labels
                .iter()
                .map(|(index, _, _, _)| chart.marks.label_text(*index))
                .chain(
                    legend
                        .iter()
                        .map(|(index, _, _)| chart.marks.label_text(*index)),
                )
                .collect();
            assert!(
                named.iter().all(|label| !label.ends_with('…')),
                "at {width}: a label was cut to a stub: {named:?}"
            );
            let total = named.len();
            named.sort();
            named.dedup();
            assert_eq!(total, named.len(), "at {width}: labels repeat: {named:?}");
            for rect in labels
                .iter()
                .map(|(_, _, rect, _)| *rect)
                .chain(legend.iter().map(|(_, _, rect)| *rect))
            {
                assert!(
                    rect.x0 >= -0.5 && rect.x1 <= width + 0.5 && rect.y1 <= 320.5,
                    "at {width}: {rect:?} leaves the chart"
                );
            }
        }
    }

    /// A chart with room names its months inside their wedges, whole, whatever the min width a
    /// shortened label would need to be worth showing.
    #[test]
    fn short_month_names_fit_their_sectors_whole() {
        let chart = placed(months(), 1000.0, &Theme::light());
        assert_eq!(chart.marks.labels.len(), 12);
        assert!(chart.marks.legend.is_empty());
    }

    /// A label reads on what it stands on: its sector's fill where it stands inside, and the
    /// page where it stands beside the circle, in both themes.
    #[test]
    fn a_label_reads_on_what_it_stands_on() {
        for theme in [Theme::light(), Theme::dark()] {
            for (data, width) in [(months(), 1000.0), (weeks(), 1000.0), (months(), 358.0)] {
                let chart = placed(data, width, &theme);
                for (index, slot, _, leader) in &chart.marks.labels {
                    let outside = leader.is_some();
                    let ink = chart.marks.label_ink(*index, *slot, outside, &theme);
                    let ground = if outside {
                        theme.tokens.colors.surfaces.base
                    } else {
                        let datum = &chart.marks.data[*index];
                        PolarAreaMarks::color(datum, *slot, &theme)
                    };
                    assert!(
                        crate::a11y::contrast_ratio(ink, ground)
                            >= crate::tokens::colors::ON_FILL_MIN_CONTRAST,
                        "at {width}: {ink:?} on {ground:?} is under 4.5:1"
                    );
                }
            }
        }
    }

    /// Weeks whose labels are too long for their wedges stand outside them with leader lines at
    /// a laptop's width, whole, rather than every one shortening to "The week…".
    #[test]
    fn long_labels_stand_outside_with_leaders_where_the_chart_has_room() {
        let chart = placed(weeks(), 1000.0, &Theme::light());
        assert_eq!(chart.marks.labels.len(), 6, "every week is named");
        assert!(
            chart.marks.legend.is_empty(),
            "no legend is needed beside them"
        );
        assert!(
            chart
                .marks
                .labels
                .iter()
                .any(|(_, _, _, leader)| leader.is_some()),
            "a label with no room inside stands outside with a leader"
        );
    }

    #[test]
    fn explicit_max_requires_positive_finite_value() {
        let mut chart = PolarAreaChart::new(vec![ChartItem::new("a", "A", 7.0)]);
        chart.marks.style.max = Some(-2.0);
        assert_eq!(chart.marks.max(), 7.0);
        chart.marks.style.max = Some(10.0);
        assert_eq!(chart.marks.max(), 10.0);
    }
}
