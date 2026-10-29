//! Interactive timeline, Gantt, and numeric range bars.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "bounded floating-point chart geometry"
)]

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use crate::paint::with_alpha;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::chart_labels::format_compact;
use crate::widget::chart_navigation::draw_title;
use crate::widget::plot::{Plot, PlotFrame, PlotItem, PlotMarks};

/// One interval. Equal start/end values render as milestones.
#[derive(Clone, Debug)]
pub struct RangeBar {
    pub id: String,
    pub label: String,
    pub start: f64,
    pub end: f64,
    pub progress: Option<f64>,
    pub color: Option<Color>,
    pub detail: String,
}

impl RangeBar {
    pub fn new(id: impl Into<String>, label: impl Into<String>, start: f64, end: f64) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            start,
            end,
            progress: None,
            color: None,
            detail: String::new(),
        }
    }
    #[must_use]
    pub fn with_progress(mut self, progress: f64) -> Self {
        self.progress = progress.is_finite().then_some(progress.clamp(0.0, 1.0));
        self
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
    fn valid(&self) -> bool {
        self.start.is_finite() && self.end.is_finite()
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent presentation switches are composable"
)]
#[derive(Clone, Debug)]
pub struct RangeStyle {
    pub range: Option<(f64, f64)>,
    pub tick_count: u8,
    pub row_height: f64,
    pub bar_height: f64,
    pub label_width: f64,
    pub corner_radius: f64,
    pub show_grid: bool,
    pub show_row_bands: bool,
    pub show_progress: bool,
    pub tooltip: bool,
}

impl Default for RangeStyle {
    fn default() -> Self {
        Self {
            range: None,
            tick_count: 6,
            row_height: 42.0,
            bar_height: 20.0,
            label_width: 112.0,
            corner_radius: 5.0,
            show_grid: true,
            show_row_bands: true,
            show_progress: true,
            tooltip: true,
        }
    }
}

pub struct RangeMarks {
    bars: Vec<RangeBar>,
    style: RangeStyle,
    axis_title: String,
    formatter: Option<Box<dyn Fn(f64) -> String>>,
    frame: PlotFrame<String>,
    hits: Vec<(String, Rect)>,
}

/// Intervals on one value axis — a timeline or Gantt chart — with
/// progress and milestones.
pub type RangeChart = Plot<RangeMarks>;

impl RangeChart {
    pub fn new(bars: Vec<RangeBar>) -> Self {
        Plot::from_marks(RangeMarks {
            bars,
            style: RangeStyle::default(),
            axis_title: String::new(),
            formatter: None,
            frame: PlotFrame::fill(140.0),
            hits: Vec::new(),
        })
    }
    #[must_use]
    pub fn with_style(mut self, style: RangeStyle) -> Self {
        self.marks.style = style;
        self
    }
    #[must_use]
    pub fn axis_titled(mut self, title: impl Into<String>) -> Self {
        self.marks.axis_title = title.into();
        self
    }
    /// Label axis ticks and intervals with `format` instead of plain numbers.
    #[must_use]
    pub fn formatted(mut self, format: impl Fn(f64) -> String + 'static) -> Self {
        self.marks.formatter = Some(Box::new(format));
        self
    }
    pub fn inspect(&mut self, id: &str) -> bool {
        let valid = self
            .marks
            .bars
            .iter()
            .any(|bar| bar.valid() && bar.id == id);
        self.inspect_item(valid.then(|| id.to_owned()))
    }
}

impl RangeMarks {
    fn height(&self) -> f32 {
        let head = if self.frame.title.is_empty() {
            54.0
        } else {
            82.0
        };
        let count = f64::from(u32::try_from(self.bars.len()).unwrap_or(u32::MAX));
        crate::num::f32_of_f64((head + count * self.style.row_height).max(140.0))
    }
    fn domain(&self) -> (f64, f64) {
        if let Some((min, max)) = self.style.range
            && min.is_finite()
            && max.is_finite()
            && max > min
        {
            return (min, max);
        }
        let (min, max) = self
            .bars
            .iter()
            .filter(|b| b.valid())
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), b| {
                (min.min(b.start.min(b.end)), max.max(b.start.max(b.end)))
            });
        if min.is_finite() && max > min {
            let pad = (max - min) * 0.04;
            (min - pad, max + pad)
        } else {
            (min.min(0.0) - 0.5, min.min(0.0) + 0.5)
        }
    }
    fn plot(&self) -> Rect {
        Rect::new(
            self.frame.rect.x0 + self.style.label_width,
            self.frame.rect.y0
                + if self.frame.title.is_empty() {
                    16.0
                } else {
                    44.0
                },
            self.frame.rect.x1 - 16.0,
            self.frame.rect.y1 - 38.0,
        )
    }
    fn x(&self, value: f64, domain: (f64, f64)) -> f64 {
        ((value - domain.0) / (domain.1 - domain.0)).mul_add(self.plot().width(), self.plot().x0)
    }
    fn format(&self, value: f64) -> String {
        self.formatter
            .as_ref()
            .map_or_else(|| format_compact(value), |f| f(value))
    }
    fn hit_mark(&self, point: Point) -> Option<&str> {
        self.hits
            .iter()
            .find(|(_, rect)| rect.inflate(5.0, 7.0).contains(point))
            .map(|(id, _)| id.as_str())
    }
}

impl PlotMarks for RangeMarks {
    type Hover = String;
    type Activated = String;
    const NAME: &'static str = "RangeChart";
    const LABEL: &'static str = "Range chart";

    fn frame(&self) -> &PlotFrame<String> {
        &self.frame
    }
    fn frame_mut(&mut self) -> &mut PlotFrame<String> {
        &mut self.frame
    }
    /// One row per bar under the axis head, unless set.
    fn layout_height(&self) -> Option<f32> {
        Some(self.frame.height.unwrap_or_else(|| self.height()))
    }
    fn hit(&self, point: Point) -> Option<String> {
        self.hit_mark(point).map(str::to_owned)
    }
    fn activation(&self, id: &String) -> Option<String> {
        Some(id.clone())
    }
    fn keyboard_items(&self) -> Vec<String> {
        let valid = self.bars.iter().filter(|bar| bar.valid());
        valid.map(|bar| bar.id.clone()).collect()
    }
    fn anchor(&self, id: &String) -> Option<Point> {
        let (_, rect) = self.hits.iter().find(|(candidate, _)| candidate == id)?;
        Some(rect.center())
    }
    fn items(&self) -> Vec<PlotItem<String>> {
        self.bars
            .iter()
            .filter(|b| b.valid())
            .map(|b| {
                PlotItem::new(
                    &b.id,
                    &b.label,
                    if b.detail.is_empty() {
                        format!("{} to {}", self.format(b.start), self.format(b.end))
                    } else {
                        b.detail.clone()
                    },
                    b.id.clone(),
                )
            })
            .collect()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "axis, intervals, milestones, progress, and inspector share a projection"
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
        let plot = self.plot();
        let domain = self.domain();
        let span = domain.1 - domain.0;
        for value in crate::charts::nice_ticks(
            domain.0,
            domain.1,
            usize::from(self.style.tick_count.max(2)),
        ) {
            let fraction = if span > 0.0 {
                (value - domain.0) / span
            } else {
                0.0
            };
            let x = fraction.mul_add(plot.width(), plot.x0);
            if self.style.show_grid {
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    with_alpha(theme.tokens.colors.text_dim, 36),
                    None,
                    &Line::new((x, plot.y0), (x, plot.y1)),
                );
            }
            tr.draw_style(
                scene,
                &self.format(value),
                theme.tokens.colors.text_dim,
                transform * Affine::translate((x - 12.0, plot.y1 + 9.0)),
                None,
                &theme.text(TextRole::Caption),
            );
        }
        if !self.axis_title.is_empty() {
            tr.draw_style(
                scene,
                &self.axis_title,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((plot.center().x - 32.0, plot.y1 + 25.0)),
                None,
                &theme.text(TextRole::Caption),
            );
        }
        self.hits.clear();
        for (index, bar) in self.bars.iter().filter(|b| b.valid()).enumerate() {
            let index = f64::from(u32::try_from(index).unwrap_or(u32::MAX));
            let row_y = plot.y0 + index * self.style.row_height;
            if self.style.show_row_bands && index.rem_euclid(2.0) >= 1.0 {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    with_alpha(theme.tokens.colors.text_dim, 13),
                    None,
                    &Rect::new(
                        self.frame.rect.x0,
                        row_y,
                        self.frame.rect.x1,
                        row_y + self.style.row_height,
                    ),
                );
            }
            tr.draw_style(
                scene,
                &bar.label,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((self.frame.rect.x0 + 10.0, row_y + 12.0)),
                Some(crate::num::f32_of_f64(
                    (self.style.label_width - 22.0).max(16.0),
                )),
                &theme.text(TextRole::Caption),
            );
            let x0 = self.x(bar.start.min(bar.end), domain);
            let x1 = self.x(bar.start.max(bar.end), domain);
            let y = row_y + (self.style.row_height - self.style.bar_height) / 2.0;
            let color = bar.color.unwrap_or(theme.tokens.colors.intent.value_fill);
            let rect = if (bar.end - bar.start).abs() <= f64::EPSILON {
                let r = self.style.bar_height * 0.42;
                let mut diamond = BezPath::new();
                diamond.move_to((x0, y));
                diamond.line_to((x0 + r, y + r));
                diamond.line_to((x0, y + r * 2.0));
                diamond.line_to((x0 - r, y + r));
                diamond.close_path();
                scene.fill(Fill::NonZero, transform, color, None, &diamond);
                Rect::new(x0 - r, y, x0 + r, y + r * 2.0)
            } else {
                let rect = Rect::new(x0, y, x1.max(x0 + 2.0), y + self.style.bar_height);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    with_alpha(color, 82),
                    None,
                    &RoundedRect::from_rect(rect, self.style.corner_radius),
                );
                if self.style.show_progress
                    && let Some(progress) = bar.progress
                {
                    let fill = Rect::new(x0, y, (x1 - x0).mul_add(progress, x0), rect.y1);
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        color,
                        None,
                        &RoundedRect::from_rect(fill, self.style.corner_radius),
                    );
                }
                if self.frame.inspection.hovered.as_deref() == Some(&bar.id) {
                    scene.stroke(
                        &Stroke::new(2.0),
                        transform,
                        theme.tokens.colors.text_primary,
                        None,
                        &RoundedRect::from_rect(rect.inflate(2.0, 2.0), self.style.corner_radius),
                    );
                }
                rect
            };
            self.hits.push((bar.id.clone(), rect));
        }
        self.follow_keyboard_anchor();
        let hovered = self
            .frame
            .inspection
            .hovered
            .as_deref()
            .and_then(|id| self.bars.iter().find(|bar| bar.id == id));
        if let Some(bar) = hovered.filter(|_| self.style.tooltip) {
            let interval = if (bar.end - bar.start).abs() <= f64::EPSILON {
                self.format(bar.start)
            } else {
                format!("{} – {}", self.format(bar.start), self.format(bar.end))
            };
            let detail = if bar.detail.is_empty() {
                interval
            } else {
                format!("{interval} · {}", bar.detail)
            };
            crate::widget::chart_labels::draw_tooltip(
                scene,
                transform,
                tr,
                theme,
                self.frame.inspection.pointer,
                self.frame.rect,
                150.0,
                (&bar.label, &detail),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn value_range_includes_reversed_intervals_unless_explicit() {
        {
            let domain = RangeChart::new(vec![RangeBar::new("a", "A", 8.0, 2.0)])
                .marks
                .domain();
            assert!(domain.0 < 2.0 && domain.1 > 8.0);
        }

        {
            let chart = RangeChart::new(Vec::new()).with_style(RangeStyle {
                range: Some((10.0, 20.0)),
                ..Default::default()
            });
            assert_eq!(chart.marks.domain(), (10.0, 20.0));
        }
    }
}
