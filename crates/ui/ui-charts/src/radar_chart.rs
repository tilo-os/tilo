//! Interactive radar/spider charts with independently scaled axes.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "polar projection is bounded floating-point geometry"
)]

use std::f64::consts::{FRAC_PI_2, TAU};

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

use crate::chart_geometry;
use crate::chart_labels::ShownLabel;
use crate::chart_legend;
use crate::chart_navigation::draw_title;
use crate::chart_readout::Readout;
use crate::plot::{Margins, Plot, PlotFrame, PlotItem, PlotMarks};
use tilo_ui::color_compat::ColorCompat;
use tilo_ui_base::chart_palette::ChartPalette;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;

#[derive(Clone, Debug)]
pub struct RadarAxis {
    pub id: String,
    pub label: String,
    pub max: f64,
}

impl RadarAxis {
    pub fn new(id: impl Into<String>, label: impl Into<String>, max: f64) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            max: max.max(f64::EPSILON),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RadarSeries {
    pub id: String,
    pub name: String,
    pub color: Color,
    /// Its place in the theme's palette, when it is coloured by that and not by `color`: the
    /// colour then follows the theme the series is drawn with.
    pub palette: Option<usize>,
    pub values: Vec<f64>,
    pub detail: String,
}

impl RadarSeries {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        color: Color,
        values: Vec<f64>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            color,
            palette: None,
            values,
            detail: String::new(),
        }
    }

    /// A series in the colour the theme's palette has at `index`, whichever theme draws it.
    pub fn from_palette(
        id: impl Into<String>,
        name: impl Into<String>,
        index: usize,
        values: Vec<f64>,
    ) -> Self {
        Self {
            palette: Some(index),
            ..Self::new(id, name, Color::TRANSPARENT, values)
        }
    }

    fn color_in(&self, theme: &Theme) -> Color {
        self.palette.map_or(self.color, |index| {
            tilo_ui_base::chart_palette::ChartPalette::from_theme(theme).categorical(index)
        })
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

#[derive(Clone, Debug)]
pub struct RadarLegendStyle {
    pub visible: bool,
    /// Let pointer and assistive activation toggle series from the legend.
    pub interactive: bool,
}

impl Default for RadarLegendStyle {
    fn default() -> Self {
        Self {
            visible: true,
            interactive: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RadarStyle {
    pub rings: u8,
    pub start_angle: f64,
    pub stroke_width: f64,
    pub marker_radius: f64,
    pub legend: RadarLegendStyle,
    pub tooltip: bool,
    pub clamp_values: bool,
}

impl Default for RadarStyle {
    fn default() -> Self {
        Self {
            rings: 5,
            start_angle: -FRAC_PI_2,
            stroke_width: 2.0,
            marker_radius: 4.0,
            legend: RadarLegendStyle::default(),
            tooltip: true,
            clamp_values: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hovered {
    series: usize,
    axis: usize,
}

pub struct RadarMarks {
    axes: Vec<RadarAxis>,
    series: Vec<RadarSeries>,
    style: RadarStyle,
    frame: PlotFrame<Hovered>,
    toggles: chart_legend::SeriesToggles,
}

pub type RadarChart = Plot<RadarMarks>;

impl RadarChart {
    pub fn new(axes: Vec<RadarAxis>, series: Vec<RadarSeries>) -> Self {
        Plot::from_marks(RadarMarks {
            axes,
            series,
            style: RadarStyle::default(),
            frame: PlotFrame::new(400.0, 160.0),
            toggles: chart_legend::SeriesToggles::default(),
        })
    }
    #[must_use]
    pub fn with_style(mut self, style: RadarStyle) -> Self {
        self.marks.style = style;
        self
    }
    pub fn set_series(&mut self, series: Vec<RadarSeries>) {
        let marks = &mut self.marks;
        marks.series = series;
        marks
            .toggles
            .retain(|id| marks.series.iter().any(|series| series.id == id));
        marks.frame.inspection.hovered = None;
    }
    /// The axes a series' readings are on, in order: the same readings stay on their axes by
    /// place.
    pub fn set_axes(&mut self, axes: Vec<RadarAxis>) {
        let marks = &mut self.marks;
        marks.axes = axes;
        marks.frame.inspection.hovered = None;
    }
    pub fn inspect(&mut self, series_id: &str, axis_id: &str) -> bool {
        let marks = &self.marks;
        let hovered = marks
            .series
            .iter()
            .enumerate()
            .find_map(|(series, values)| {
                (!marks.toggles.is_hidden(&values.id) && values.id == series_id)
                    .then(|| {
                        marks
                            .axes
                            .iter()
                            .enumerate()
                            .find(|(axis, spec)| {
                                spec.id == axis_id
                                    && values
                                        .values
                                        .get(*axis)
                                        .is_some_and(|value| value.is_finite())
                            })
                            .map(|(axis, _)| Hovered { series, axis })
                    })
                    .flatten()
            });
        self.inspect_item(hovered)
    }
}

impl RadarMarks {
    /// Where the legend's entries stand: under the title, as on every chart. `None` while the
    /// chart shows no legend.
    fn legend_area(&self) -> Option<Rect> {
        self.style.legend.visible.then(|| self.frame.legend_area())
    }

    /// The title and legend over the web, and its axes' labels around it, a step from the
    /// outer ring.
    fn measure_margins(&self, tr: &mut TextRenderer, theme: &Theme) -> Margins {
        let legend_rows = if self.style.legend.visible {
            self.frame
                .legend_rows(self.series.iter().map(|series| series.name.as_str()))
        } else {
            0
        };
        let step = crate::plot::step(theme);
        let line = crate::plot::caption_line(theme);
        let widest =
            crate::plot::widest(tr, theme, self.axes.iter().map(|axis| axis.label.as_str()));
        let mut margins = Margins::header(theme, &self.frame.title, legend_rows);
        margins.top += line + step;
        margins.bottom = line + step;
        margins.left = (widest + step).min(self.frame.rect.width() / 4.0);
        margins.right = margins.left;
        margins
    }

    fn geometry(&self) -> (Point, f64) {
        let area = self.frame.inside();
        (area.center(), area.width().min(area.height()) / 2.0)
    }

    /// `axis`'s label as the chart shows it, outside the ring on the axis's side: whole on one
    /// line where its room holds it; otherwise whole over two lines where it stands beside the
    /// ring, the side having the chart's height to stack them; otherwise shortened to whole
    /// words of itself on either side of one ellipsis, never splitting a word; and left out
    /// where none of those fits, the readout and the accessible tree still naming the axis.
    fn axis_label(&self, tr: &mut TextRenderer, theme: &Theme, axis: usize) -> Option<ShownLabel> {
        let label = self.axes.get(axis)?.label.trim();
        if label.is_empty() {
            return None;
        }
        let style = theme.text(TextRole::Caption);
        let step = crate::plot::step(theme);
        let (center, radius) = self.geometry();
        let angle = self.angle(axis);
        let (cos, sin) = (angle.cos(), angle.sin());
        let at = chart_geometry::polar(center, radius + step, angle);
        let beside = cos.abs() > 0.3;
        let room = if beside {
            (self.frame.margins.left - step).max(0.0)
        } else {
            self.frame.rect.width()
        };
        let measure = |tr: &mut TextRenderer, text: &str| {
            let (w, h) = tr.measure_style(text, None, &style);
            (f64::from(w), f64::from(h))
        };
        let (w, h) = measure(tr, label);
        let lines = if w <= room {
            vec![(label.to_owned(), w, h)]
        } else {
            let wrapped = if beside {
                crate::chart_labels::wrapped_labels(tr, label, &style)
                    .into_iter()
                    .find(|block| block.width <= room)
            } else {
                None
            };
            if let Some(block) = wrapped {
                block.lines
            } else {
                let cut = crate::chart_labels::shortened_labels(tr, label, &style)
                    .into_iter()
                    .find(|(_, w, _)| *w <= room)?;
                vec![cut]
            }
        };
        let height: f64 = lines.iter().map(|(_, _, h)| *h).sum();
        let mut y = if sin < -0.3 {
            at.y - height
        } else if sin > 0.3 {
            at.y
        } else {
            at.y - height / 2.0
        };
        let mut placed = Vec::with_capacity(lines.len());
        for (text, w, h) in lines {
            let x = if cos > 0.3 {
                at.x
            } else if cos < -0.3 {
                at.x - w
            } else {
                at.x - w / 2.0
            };
            placed.push((text, Rect::new(x, y, x + w, y + h)));
            y += h;
        }
        Some(ShownLabel {
            lines: placed,
            turn: 0.0,
        })
    }

    fn angle(&self, axis: usize) -> f64 {
        self.style.start_angle
            + f64::from(u32::try_from(axis).unwrap_or(u32::MAX))
                / f64::from(u32::try_from(self.axes.len().max(1)).unwrap_or(u32::MAX))
                * TAU
    }
    fn value_point(&self, series: &RadarSeries, axis: usize) -> Option<Point> {
        let axis_spec = self.axes.get(axis)?;
        let value = *series.values.get(axis)?;
        if !value.is_finite() {
            return None;
        }
        let fraction = if self.style.clamp_values {
            (value / axis_spec.max).clamp(0.0, 1.0)
        } else {
            value / axis_spec.max
        };
        let (center, radius) = self.geometry();
        Some(chart_geometry::polar(
            center,
            radius * fraction,
            self.angle(axis),
        ))
    }
    fn polygon(&self, series: &RadarSeries, fraction: f64) -> BezPath {
        let (center, radius) = self.geometry();
        let mut path = BezPath::new();
        for axis in 0..self.axes.len() {
            let point = if (fraction - 1.0).abs() <= f64::EPSILON {
                self.value_point(series, axis).unwrap_or(center)
            } else {
                chart_geometry::polar(center, radius * fraction, self.angle(axis))
            };
            if axis == 0 {
                path.move_to(point);
            } else {
                path.line_to(point);
            }
        }
        path.close_path();
        path
    }
}

impl PlotMarks for RadarMarks {
    type Hover = Hovered;
    type Activated = (String, String);
    const NAME: &'static str = "RadarChart";
    const LABEL: &'static str = "Radar chart";

    fn frame(&self) -> &PlotFrame<Hovered> {
        &self.frame
    }
    fn frame_mut(&mut self) -> &mut PlotFrame<Hovered> {
        &mut self.frame
    }
    fn hit(&self, pointer: Point) -> Option<Hovered> {
        let mut nearest: Option<(f64, Hovered)> = None;
        for (series_index, series) in self.series.iter().enumerate() {
            if self.toggles.is_hidden(&series.id) {
                continue;
            }
            for axis in 0..self.axes.len() {
                let Some(point) = self.value_point(series, axis) else {
                    continue;
                };
                let distance = point.distance(pointer);
                if distance <= 22.0 && nearest.is_none_or(|current| distance < current.0) {
                    nearest = Some((
                        distance,
                        Hovered {
                            series: series_index,
                            axis,
                        },
                    ));
                }
            }
        }
        nearest.map(|(_, hovered)| hovered)
    }
    fn activation(&self, hovered: &Hovered) -> Option<(String, String)> {
        let series = self.series.get(hovered.series)?;
        let axis = self.axes.get(hovered.axis)?;
        Some((series.id.clone(), axis.id.clone()))
    }
    fn keyboard_items(&self) -> Vec<Hovered> {
        self.series
            .iter()
            .enumerate()
            .filter(|(_, series)| !self.toggles.is_hidden(&series.id))
            .flat_map(|(series, values)| {
                self.axes.iter().enumerate().filter_map(move |(axis, _)| {
                    values
                        .values
                        .get(axis)
                        .filter(|value| value.is_finite())
                        .map(|_| Hovered { series, axis })
                })
            })
            .collect()
    }
    fn anchor(&self, hovered: &Hovered) -> Option<Point> {
        self.value_point(self.series.get(hovered.series)?, hovered.axis)
    }
    fn items(&self) -> Vec<PlotItem<(String, String)>> {
        self.series
            .iter()
            .filter(|series| !self.toggles.is_hidden(&series.id))
            .flat_map(|series| {
                self.axes.iter().enumerate().filter_map(|(index, axis)| {
                    let value = series.values.get(index).filter(|value| value.is_finite())?;
                    Some(PlotItem::new(
                        format!("{}:{}", series.id, axis.id),
                        format!("{}, {}", series.name, axis.label),
                        format!("{value} of {}. {}", axis.max, series.detail),
                        (series.id.clone(), axis.id.clone()),
                    ))
                })
            })
            .collect()
    }
    fn series(&self) -> Vec<(&str, &str)> {
        self.series
            .iter()
            .map(|series| (series.id.as_str(), series.name.as_str()))
            .collect()
    }
    fn legend_interactive(&self) -> bool {
        self.style.legend.interactive
    }
    fn toggles(&self) -> Option<&chart_legend::SeriesToggles> {
        Some(&self.toggles)
    }
    fn toggles_mut(&mut self) -> Option<&mut chart_legend::SeriesToggles> {
        Some(&mut self.toggles)
    }
    fn series_of(&self, hovered: &Hovered) -> Option<&str> {
        Some(self.series.get(hovered.series)?.id.as_str())
    }
    fn place(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        self.frame.margins = self.measure_margins(tr, theme);
        self.toggles.place(
            self.series
                .iter()
                .map(|series| (series.id.as_str(), series.name.as_str())),
            self.legend_area(),
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "polar grid, series, legend and inspector share one paint pass"
    )]
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.axes.len() < 3 {
            return;
        }
        let (center, radius) = self.geometry();
        self.follow_keyboard_anchor();
        draw_title(
            scene,
            transform,
            tr,
            theme,
            self.frame.rect,
            &self.frame.title,
        );
        let grid = Color::from_rgba8(
            theme.tokens.colors.border_standard.r(),
            theme.tokens.colors.border_standard.g(),
            theme.tokens.colors.border_standard.b(),
            130,
        );
        let rings = self.style.rings.clamp(1, 10);
        let dummy = RadarSeries::new("", "", Color::TRANSPARENT, vec![]);
        for ring in 1..=rings {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                grid,
                None,
                &self.polygon(&dummy, f64::from(ring) / f64::from(rings)),
            );
        }
        for axis in 0..self.axes.len() {
            let outer = chart_geometry::polar(center, radius, self.angle(axis));
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                grid,
                None,
                &Line::new(center, outer),
            );
            // A label stands outside the ring on its axis's side, reading away from the web:
            // one to the right starts at the axis, one to the left ends there, one above or
            // below is centred on it.
            if let Some(label) = self.axis_label(tr, theme, axis) {
                let style = theme.text(TextRole::Caption);
                for (line, at) in &label.lines {
                    tr.draw_style(
                        scene,
                        line,
                        theme.tokens.colors.text_dim,
                        transform * Affine::translate((at.x0, at.y0)),
                        None,
                        &style,
                    );
                }
            }
        }
        for (series_index, series) in self.series.iter().enumerate() {
            if self.toggles.is_hidden(&series.id) {
                continue;
            }
            let color = series.color_in(theme);
            let path = self.polygon(series, 1.0);
            scene.fill(
                Fill::NonZero,
                transform,
                ChartPalette::area_of(color),
                None,
                &path,
            );
            scene.stroke(
                &Stroke::new(self.style.stroke_width),
                transform,
                color,
                None,
                &path,
            );
            for axis in 0..self.axes.len() {
                if let Some(point) = self.value_point(series, axis) {
                    let selected = self.frame.inspection.hovered.is_some_and(|hovered| {
                        hovered.series == series_index && hovered.axis == axis
                    });
                    if selected {
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            ChartPalette::area_of(color),
                            None,
                            &Circle::new(
                                point,
                                self.style.marker_radius + f64::from(theme.tokens.spacing.s2),
                            ),
                        );
                    }
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        color,
                        None,
                        &Circle::new(point, self.style.marker_radius),
                    );
                }
            }
        }
        if self.style.legend.visible {
            self.toggles.paint(
                scene,
                transform,
                tr,
                theme,
                self.series.iter().map(|series| {
                    (
                        series.id.as_str(),
                        series.name.as_str(),
                        series.color_in(theme),
                    )
                }),
            );
        }
    }

    fn readout(&self, _theme: &Theme) -> Option<Readout> {
        if let Some(hovered) = self.frame.inspection.hovered.filter(|_| self.style.tooltip)
            && let (Some(series), Some(axis)) =
                (self.series.get(hovered.series), self.axes.get(hovered.axis))
        {
            let value = series.values.get(hovered.axis).copied().unwrap_or_default();
            let title = format!("{} · {}", series.name, axis.label);
            let detail = format!("{value:.2} of {:.2}", axis.max);
            return Some(Readout::of_mark(
                &title,
                &detail,
                self.frame.inspection.pointer,
                self.frame.rect,
            ));
        }
        None
    }
}

mod radar_chart_component;
pub use radar_chart_component::{
    RadarAxisProps, RadarChartProps, RadarSeriesProps, axes_description, radar_series_description,
};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn radar_axes_and_legend_validate_and_expose_state() {
        {
            assert!(RadarAxis::new("x", "X", 0.0).max > 0.0);
        }

        {
            let axes = vec![
                RadarAxis::new("a", "A", 10.0),
                RadarAxis::new("b", "B", 10.0),
                RadarAxis::new("c", "C", 10.0),
            ];
            let mut chart = RadarChart::new(
                axes,
                vec![RadarSeries::new(
                    "current",
                    "Current",
                    Color::from_rgb8(1, 2, 3),
                    vec![1.0, 2.0, 3.0],
                )],
            );
            assert!(chart.set_series_visible("current", false));
            assert!(!chart.is_series_visible("current"));
            assert!(!chart.inspect("current", "a"));
            assert_eq!(
                chart.take_visibility_changed(),
                Some(("current".to_owned(), false))
            );
            assert!(chart.toggle_series("current"));
            assert!(chart.inspect("current", "a"));
        }
    }

    /// The gallery's longest axis names, far wider than the room beside a radar's ring.
    fn long_axes() -> Vec<RadarAxis> {
        vec![
            RadarAxis::new("answer", "Time to the first useful answer", 100.0),
            RadarAxis::new("latency", "P95 request latency at peak load", 100.0),
            RadarAxis::new("tickets", "Support tickets closed per day", 100.0),
            RadarAxis::new("docs", "Documentation pages updated", 100.0),
        ]
    }

    /// An axis's name stays readable wherever the chart stands: the chart shows it whole, on
    /// one line or over two, or as whole words of itself on either side of one ellipsis — a cut
    /// never splits a word. Where none of those fits it is left out, and the readout and the
    /// accessible tree still name the axis whole.
    #[test]
    fn axis_labels_stay_readable_at_every_width() {
        let axes = long_axes();
        let series = vec![RadarSeries::new(
            "after",
            "After the caching change",
            Color::from_rgb8(1, 2, 3),
            vec![68.0, 74.0, 52.0, 41.0],
        )];
        for theme in [Theme::light(), Theme::dark()] {
            let mut tr = TextRenderer::new();
            for width in [1000.0, 358.0] {
                let mut chart = RadarChart::new(axes.clone(), series.clone());
                // The height a radar takes on a page; the width is the one under test.
                chart.marks.frame.rect = Rect::new(0.0, 0.0, width, 400.0);
                chart.marks.place(&mut tr, &theme);
                let items = chart.marks.items();
                let mut shown = 0;
                for axis in &axes {
                    // However short the chart draws it, the whole name is still read by the
                    // readout and the accessible tree.
                    assert!(
                        items
                            .iter()
                            .any(|item| item.name.contains(axis.label.as_str())),
                        "{width}: {:?} is named nowhere",
                        axis.label
                    );
                }
                for axis in 0..axes.len() {
                    let Some(shown_label) = chart.marks.axis_label(&mut tr, &theme, axis) else {
                        continue;
                    };
                    shown += 1;
                    let text = shown_label.text();
                    let label = axes.get(axis).expect("the axis").label.as_str();
                    assert!(
                        crate::chart_labels::reads_as_whole_words(label, &text),
                        "{width}: {label:?} shows {text:?}, which is not whole words of it"
                    );
                }
                // Pointing at a mark on any axis reads that axis's whole name.
                for axis in 0..axes.len() {
                    let label = axes.get(axis).expect("the axis").label.as_str();
                    chart.marks.frame.inspection.hovered = Some(Hovered { series: 0, axis });
                    let readout = chart.marks.readout(&theme).expect("the mark reads out");
                    assert!(
                        readout.title.contains(label),
                        "{width}: the readout names {label:?} as {:?}",
                        readout.title
                    );
                }
                // At the phone's width the room beside the dial holds three of these names: the
                // longest word left over has no whole-word reading and no line break that fits,
                // so that axis is named by its readout and the accessible tree alone.
                let least = if width <= 358.0 { 3 } else { axes.len() };
                assert!(
                    shown >= least,
                    "{width}: the chart names only {shown} of its {} axes",
                    axes.len()
                );
            }
        }
    }
}
