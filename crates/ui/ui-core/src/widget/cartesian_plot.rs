//! Highly configurable interactive Cartesian plots.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "plot projection and vector geometry are bounded floating-point arithmetic"
)]

use crate::time::Instant;
use std::cell::RefCell;
use tilo_sort::CompactSort as _;

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use crate::chart_palette::ChartPalette;
use crate::paint::with_alpha;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::chart_labels::format_compact;
use crate::widget::chart_legend;
use crate::widget::chart_readout::{Highlight, Readout};
use crate::widget::plot::{Plot, PlotFrame, PlotItem, PlotMarks};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AxisRange {
    Auto,
    Fixed { min: f64, max: f64 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AxisScale {
    #[default]
    Linear,
    Log10,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AxisTick {
    pub value: f64,
    pub label: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AxisLabelFormat {
    #[default]
    Auto,
    Decimal(u8),
    Compact,
    Percent(u8),
    Bytes(u8),
    /// Bytes stored or transferred, in SI decimal units with the precision
    /// [`crate::format::size`] reads in (`500 MB`, `1.5 GB`).
    Size,
    Scientific(u8),
    Duration(u8),
    Currency {
        symbol: char,
        places: u8,
    },
}

impl AxisTick {
    pub fn new(value: f64, label: impl Into<String>) -> Self {
        Self {
            value,
            label: label.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlotAxis {
    pub range: AxisRange,
    pub scale: AxisScale,
    pub ticks: Vec<AxisTick>,
    pub tick_count: u8,
    pub title: String,
    pub grid: bool,
    pub include_zero: bool,
    pub label_format: AxisLabelFormat,
    pub min_tick_spacing: f64,
}

impl Default for PlotAxis {
    fn default() -> Self {
        Self {
            range: AxisRange::Auto,
            scale: AxisScale::Linear,
            ticks: Vec::new(),
            tick_count: 5,
            title: String::new(),
            grid: true,
            include_zero: false,
            label_format: AxisLabelFormat::Auto,
            min_tick_spacing: 72.0,
        }
    }
}

impl PlotAxis {
    #[must_use]
    pub fn fixed(mut self, min: f64, max: f64) -> Self {
        self.range = AxisRange::Fixed { min, max };
        self
    }

    #[must_use]
    pub fn logarithmic(mut self) -> Self {
        self.scale = AxisScale::Log10;
        self.include_zero = false;
        self
    }

    #[must_use]
    pub fn with_ticks(mut self, ticks: impl IntoIterator<Item = AxisTick>) -> Self {
        self.ticks = ticks.into_iter().collect();
        self
    }

    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    #[must_use]
    pub fn formatted(mut self, format: AxisLabelFormat) -> Self {
        self.label_format = format;
        self
    }

    #[must_use]
    pub fn with_tick_density(mut self, maximum: u8, min_spacing: f64) -> Self {
        self.tick_count = maximum.clamp(2, 20);
        self.min_tick_spacing = min_spacing.max(24.0);
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlotDatum {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub radius: f64,
    pub label: String,
    pub detail: String,
}

impl PlotDatum {
    pub fn new(id: impl Into<String>, x: f64, y: f64) -> Self {
        Self {
            id: id.into(),
            x,
            y,
            radius: 5.0,
            label: String::new(),
            detail: String::new(),
        }
    }

    /// Create an explicit missing sample. Connected series break at missing
    /// samples unless their series opts into connecting gaps.
    pub fn missing(id: impl Into<String>, x: f64) -> Self {
        Self::new(id, x, f64::NAN)
    }

    #[must_use]
    pub fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SeriesKind {
    Line,
    Smooth,
    Step,
    Area,
    SmoothArea,
    StepArea,
    Scatter,
    Bubble,
    Bar { width: f64 },
    Lollipop,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MarkerShape {
    #[default]
    Circle,
    Square,
    Diamond,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlotSeries {
    pub id: String,
    pub name: String,
    pub color: Color,
    /// Its place in the theme's palette, when it is coloured by that and not by `color`: the
    /// colour then follows the theme the series is drawn with.
    pub palette: Option<usize>,
    pub kind: SeriesKind,
    pub data: Vec<PlotDatum>,
    pub stroke_width: f64,
    pub marker_radius: f64,
    pub line_dash: Vec<f64>,
    pub fill_alpha: u8,
    pub marker_shape: MarkerShape,
    pub bar_radius: f64,
    /// Enables logarithmic-time pointer candidate lookup. The caller promises
    /// finite, ascending X values and should keep that invariant on updates.
    pub x_sorted: bool,
    pub connect_gaps: bool,
    pub show_markers: Option<bool>,
}

impl PlotSeries {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        color: Color,
        data: Vec<PlotDatum>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            color,
            palette: None,
            kind: SeriesKind::Line,
            data,
            stroke_width: 2.0,
            marker_radius: 4.0,
            line_dash: Vec::new(),
            fill_alpha: 52,
            marker_shape: MarkerShape::Circle,
            bar_radius: 4.0,
            x_sorted: false,
            connect_gaps: false,
            show_markers: None,
        }
    }

    /// A series in the colour the theme's palette has at `index`, whichever theme draws it.
    pub fn from_palette(
        id: impl Into<String>,
        name: impl Into<String>,
        index: usize,
        data: Vec<PlotDatum>,
    ) -> Self {
        Self {
            palette: Some(index),
            ..Self::new(id, name, Color::TRANSPARENT, data)
        }
    }

    /// The colour it is drawn in, in `theme`: its own, or the theme's palette's at its place.
    pub fn color_in(&self, theme: &Theme) -> Color {
        self.palette.map_or(self.color, |index| {
            crate::chart_palette::ChartPalette::from_theme(theme).categorical(index)
        })
    }

    #[must_use]
    pub fn kind(mut self, kind: SeriesKind) -> Self {
        self.kind = kind;
        self
    }

    #[must_use]
    pub fn stroke_width(mut self, width: f64) -> Self {
        self.stroke_width = width.max(0.5);
        self
    }

    #[must_use]
    pub fn marker_radius(mut self, radius: f64) -> Self {
        self.marker_radius = radius.max(1.0);
        self
    }

    #[must_use]
    pub fn dashed(mut self, pattern: impl IntoIterator<Item = f64>) -> Self {
        self.line_dash = pattern
            .into_iter()
            .filter(|length| length.is_finite() && *length > 0.0)
            .collect();
        self
    }

    #[must_use]
    pub fn fill_alpha(mut self, alpha: u8) -> Self {
        self.fill_alpha = alpha;
        self
    }

    #[must_use]
    pub fn marker_shape(mut self, shape: MarkerShape) -> Self {
        self.marker_shape = shape;
        self
    }

    #[must_use]
    pub fn bar_radius(mut self, radius: f64) -> Self {
        self.bar_radius = radius.max(0.0);
        self
    }

    #[must_use]
    pub fn sorted_by_x(mut self) -> Self {
        self.x_sorted = true;
        self
    }

    #[must_use]
    pub fn connect_gaps(mut self, connect: bool) -> Self {
        self.connect_gaps = connect;
        self
    }

    #[must_use]
    pub fn show_markers(mut self, show: bool) -> Self {
        self.show_markers = Some(show);
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum CartesianAnnotationKind {
    XLine(f64),
    YLine(f64),
    XBand { start: f64, end: f64 },
    YBand { start: f64, end: f64 },
    Point { x: f64, y: f64 },
}

/// Presentation for an analytical annotation. Annotations deliberately use
/// data coordinates, so they remain correct through resize, zoom, and pan.
#[derive(Clone, Debug, PartialEq)]
pub struct CartesianAnnotation {
    pub id: String,
    pub kind: CartesianAnnotationKind,
    pub label: String,
    pub detail: String,
    pub color: Color,
    pub stroke_width: f64,
    pub dashed: bool,
    pub fill_alpha: u8,
    pub interactive: bool,
}

impl CartesianAnnotation {
    pub fn x_line(id: impl Into<String>, value: f64, color: Color) -> Self {
        Self::new(id, CartesianAnnotationKind::XLine(value), color)
    }

    pub fn y_line(id: impl Into<String>, value: f64, color: Color) -> Self {
        Self::new(id, CartesianAnnotationKind::YLine(value), color)
    }

    pub fn x_band(id: impl Into<String>, start: f64, end: f64, color: Color) -> Self {
        Self::new(id, CartesianAnnotationKind::XBand { start, end }, color)
    }

    pub fn point(id: impl Into<String>, x: f64, y: f64, color: Color) -> Self {
        Self::new(id, CartesianAnnotationKind::Point { x, y }, color)
    }

    fn new(id: impl Into<String>, kind: CartesianAnnotationKind, color: Color) -> Self {
        Self {
            id: id.into(),
            kind,
            label: String::new(),
            detail: String::new(),
            color,
            stroke_width: 1.5,
            dashed: true,
            fill_alpha: 30,
            interactive: true,
        }
    }

    #[must_use]
    pub fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    #[must_use]
    pub fn stroke_width(mut self, width: f64) -> Self {
        self.stroke_width = width.max(0.5);
        self
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    #[must_use]
    pub fn solid(mut self) -> Self {
        self.dashed = false;
        self
    }

    #[must_use]
    pub fn fill_alpha(mut self, alpha: u8) -> Self {
        self.fill_alpha = alpha;
        self
    }

    #[must_use]
    pub fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CartesianZoom {
    #[default]
    Disabled,
    X,
    XY,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CartesianPan {
    #[default]
    Disabled,
    X,
    XY,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CartesianBrush {
    #[default]
    Disabled,
    X,
    XY,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CartesianTooltipAnchor {
    #[default]
    Pointer,
    Datum,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CartesianBrushSelection {
    pub x: AxisRange,
    pub y: Option<AxisRange>,
}

#[derive(Clone, Debug)]
pub struct CartesianTooltipStyle {
    pub enabled: bool,
    pub anchor: CartesianTooltipAnchor,
    pub show_coordinates: bool,
    /// Pointing anywhere over the plot reads every line, area, step or bar series at the nearest
    /// x position, in one card; scatter and bubble series read the point under the pointer.
    pub shared: bool,
}

impl Default for CartesianTooltipStyle {
    fn default() -> Self {
        Self {
            enabled: true,
            anchor: CartesianTooltipAnchor::Pointer,
            show_coordinates: false,
            shared: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CartesianRenderingStyle {
    pub show_crosshair: bool,
    pub show_markers_on_lines: bool,
    /// Reduce dense connected series to a spike-preserving pixel budget.
    /// Scatter, bubble, bar, and lollipop series are never sampled.
    pub adaptive_sampling: bool,
    pub max_points_per_pixel: f64,
}

impl Default for CartesianRenderingStyle {
    fn default() -> Self {
        Self {
            show_crosshair: true,
            show_markers_on_lines: false,
            adaptive_sampling: true,
            max_points_per_pixel: 2.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CartesianGesturePolicy {
    /// Require Ctrl/Command for wheel zoom so embedded plots remain
    /// scroll-friendly. Disable only in a dedicated chart viewport.
    pub zoom_modifier_required: bool,
    /// Require Alt while beginning a pointer pan so charts in scrollable
    /// layouts do not steal ordinary drags.
    pub pan_modifier_required: bool,
    pub brush_modifier_required: bool,
}

impl Default for CartesianGesturePolicy {
    fn default() -> Self {
        Self {
            zoom_modifier_required: true,
            pan_modifier_required: true,
            brush_modifier_required: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CartesianAnimationStyle {
    pub enabled: bool,
}

impl Default for CartesianAnimationStyle {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Debug)]
pub struct CartesianStyle {
    pub x_axis: PlotAxis,
    pub y_axis: PlotAxis,
    /// The series' legend, under the title as on every chart.
    pub show_legend: bool,
    pub rendering: CartesianRenderingStyle,
    pub tooltip: CartesianTooltipStyle,
    pub plot_padding: f64,
    pub hit_radius: f64,
    pub zoom: CartesianZoom,
    pub pan: CartesianPan,
    pub gestures: CartesianGesturePolicy,
    /// Smallest visible fraction of the original data domain.
    pub min_zoom_fraction: f64,
    pub interactive_legend: bool,
    pub brush: CartesianBrush,
    pub animation: CartesianAnimationStyle,
}

impl Default for CartesianStyle {
    fn default() -> Self {
        Self {
            x_axis: PlotAxis::default(),
            y_axis: PlotAxis::default(),
            show_legend: true,
            rendering: CartesianRenderingStyle::default(),
            tooltip: CartesianTooltipStyle::default(),
            plot_padding: 12.0,
            hit_radius: 28.0,
            zoom: CartesianZoom::Disabled,
            pan: CartesianPan::Disabled,
            gestures: CartesianGesturePolicy::default(),
            min_zoom_fraction: 0.01,
            interactive_legend: true,
            brush: CartesianBrush::Disabled,
            animation: CartesianAnimationStyle::default(),
        }
    }
}

impl CartesianStyle {
    /// Configure responsive min/max sampling for connected series. Values are
    /// clamped to a practical range; pass `false` for exact path rendering.
    #[must_use]
    pub fn with_adaptive_sampling(mut self, enabled: bool, points_per_pixel: f64) -> Self {
        self.rendering.adaptive_sampling = enabled;
        self.rendering.max_points_per_pixel = if points_per_pixel.is_finite() {
            points_per_pixel.clamp(0.25, 16.0)
        } else {
            2.0
        };
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Domain {
    min: f64,
    max: f64,
}

struct AnnotationPaintContext<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text: &'a mut TextRenderer,
    theme: &'a Theme,
    plot: Rect,
    x_domain: Domain,
    y_domain: Domain,
}

#[derive(Clone, Debug)]
struct DomainCache {
    x_axis: PlotAxis,
    y_axis: PlotAxis,
    x: Option<Domain>,
    y: Option<Domain>,
}

impl Domain {
    fn normalized(min: f64, max: f64) -> Self {
        if min.is_finite() && max.is_finite() && max > min {
            Self { min, max }
        } else {
            let center = if min.is_finite() { min } else { 0.0 };
            Self {
                min: center - 0.5,
                max: center + 0.5,
            }
        }
    }

    fn from_range(range: AxisRange) -> Option<Self> {
        match range {
            AxisRange::Fixed { min, max } if min.is_finite() && max.is_finite() && max > min => {
                Some(Self { min, max })
            }
            AxisRange::Auto | AxisRange::Fixed { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CartesianHover {
    Datum { series: usize, datum: usize },
    Annotation(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub enum CartesianActivation {
    Datum { series: String, datum: String },
    Annotation(String),
}

pub struct CartesianMarks {
    frame: PlotFrame<CartesianHover>,
    series: Vec<PlotSeries>,
    annotations: Vec<CartesianAnnotation>,
    style: CartesianStyle,
    x_view: Option<Domain>,
    y_view: Option<Domain>,
    view_changed: bool,
    toggles: chart_legend::SeriesToggles,
    brush_start: Option<Point>,
    brush_current: Point,
    brushed: Option<CartesianBrushSelection>,
    pan_last: Option<Point>,
    transition_from: Option<Vec<PlotSeries>>,
    transition_started: Instant,
    reduce_motion: bool,
    domain_cache: RefCell<Option<DomainCache>>,
    bands: Bands,
}

/// The space around a plot: tick labels, axis titles, the chart's title and
/// its legend, each from the theme's type and spacing and measured text. The
/// layout places them; all zero until it has.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Bands {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
    legend_top: f64,
    gap: f64,
    tick_line: f64,
}

pub type CartesianPlot = Plot<CartesianMarks>;

mod cartesian_plot_component;
pub use cartesian_plot_component::{
    CartesianPlotProps, Curve, Marker, PlotPoints, points_description,
};

impl CartesianPlot {
    pub fn new(series: Vec<PlotSeries>) -> Self {
        Plot::from_marks(CartesianMarks {
            frame: PlotFrame::new(320.0, 100.0),
            series,
            annotations: Vec::new(),
            style: CartesianStyle::default(),
            x_view: None,
            y_view: None,
            view_changed: false,
            toggles: chart_legend::SeriesToggles::default(),
            brush_start: None,
            brush_current: Point::ZERO,
            brushed: None,
            pan_last: None,
            transition_from: None,
            transition_started: crate::time::now(),
            reduce_motion: false,
            domain_cache: RefCell::new(None),
            bands: Bands::default(),
        })
    }

    #[must_use]
    pub fn with_style(mut self, style: CartesianStyle) -> Self {
        self.marks.style = style;
        self
    }

    /// The series it draws, in order.
    #[must_use]
    pub fn series(&self) -> &[PlotSeries] {
        &self.marks.series
    }
    #[must_use]
    pub fn with_annotations(
        mut self,
        annotations: impl IntoIterator<Item = CartesianAnnotation>,
    ) -> Self {
        self.marks.annotations = annotations.into_iter().collect();
        self
    }
    pub fn annotations(&self) -> &[CartesianAnnotation] {
        &self.marks.annotations
    }
    pub fn annotations_mut(&mut self) -> &mut Vec<CartesianAnnotation> {
        &mut self.marks.annotations
    }
    pub fn set_series(&mut self, series: Vec<PlotSeries>) {
        self.mark_dirty();
        let marks = &mut self.marks;
        if marks.series == series {
            return;
        }
        let previous = std::mem::replace(&mut marks.series, series);
        if marks.style.animation.enabled && !marks.reduce_motion {
            marks.transition_from = Some(previous);
            marks.transition_started = crate::time::now();
        } else {
            marks.transition_from = None;
        }
        marks
            .toggles
            .retain(|id| marks.series.iter().any(|series| series.id == id));
        marks.frame.inspection.clear();
        marks.invalidate_domains();
    }
    pub fn set_series_immediate(&mut self, series: Vec<PlotSeries>) {
        self.mark_dirty();
        let marks = &mut self.marks;
        marks.series = series;
        marks.transition_from = None;
        marks
            .toggles
            .retain(|id| marks.series.iter().any(|series| series.id == id));
        marks.frame.inspection.clear();
        marks.invalidate_domains();
    }
    pub fn finish_animation(&mut self) {
        self.marks.transition_from = None;
    }
    pub fn is_animating(&self) -> bool {
        self.marks.is_animating()
    }
    /// Inspects the datum `datum_id` of `series_id` as the keyboard would.
    pub fn inspect(&mut self, series_id: &str, datum_id: &str) -> bool {
        let marks = &self.marks;
        let target = marks
            .series
            .iter()
            .enumerate()
            .find(|(_, series)| !marks.toggles.is_hidden(&series.id) && series.id == series_id)
            .and_then(|(series, values)| {
                values
                    .data
                    .iter()
                    .position(|datum| datum.id == datum_id)
                    .map(|datum| CartesianHover::Datum { series, datum })
            });
        self.inspect_item(target)
    }
    /// Inspects the interactive annotation `id` as a click would.
    pub fn inspect_annotation(&mut self, id: &str) -> bool {
        let index = self
            .marks
            .annotations
            .iter()
            .position(|annotation| annotation.id == id && annotation.interactive);
        self.inspect_item(index.map(CartesianHover::Annotation))
    }
    pub fn take_brush_selection(&mut self) -> Option<CartesianBrushSelection> {
        self.marks.brushed.take()
    }
    pub fn take_view_changed(&mut self) -> bool {
        std::mem::take(&mut self.marks.view_changed)
    }
    pub fn reset_view(&mut self) {
        self.marks.x_view = None;
        self.marks.y_view = None;
        self.marks.view_changed = true;
        self.marks.frame.inspection.clear();
    }
    pub fn viewport(&self) -> (Option<AxisRange>, Option<AxisRange>) {
        let range = |domain: Option<Domain>| {
            domain.map(|domain| AxisRange::Fixed {
                min: domain.min,
                max: domain.max,
            })
        };
        (range(self.marks.x_view), range(self.marks.y_view))
    }
    pub fn set_viewport(&mut self, x: Option<AxisRange>, y: Option<AxisRange>) -> bool {
        self.mark_dirty();
        let marks = &mut self.marks;
        let parse = |range: Option<AxisRange>, scale: AxisScale| match range {
            None | Some(AxisRange::Auto) => Ok(None),
            Some(range) => Domain::from_range(range)
                .filter(|domain| {
                    CartesianMarks::transform(domain.min, scale).is_some()
                        && CartesianMarks::transform(domain.max, scale).is_some()
                })
                .map(Some)
                .ok_or(()),
        };
        let (Ok(x), Ok(y)) = (
            parse(x, marks.style.x_axis.scale),
            parse(y, marks.style.y_axis.scale),
        ) else {
            return false;
        };
        if marks.x_view.map(|d| (d.min, d.max)) == x.map(|d| (d.min, d.max))
            && marks.y_view.map(|d| (d.min, d.max)) == y.map(|d| (d.min, d.max))
        {
            return false;
        }
        marks.x_view = x;
        marks.y_view = y;
        marks.view_changed = true;
        marks.frame.inspection.clear();
        true
    }
    pub fn pan_view_fraction(&mut self, x_fraction: f64, y_fraction: f64) -> bool {
        let (Some(x), Some(y)) = (
            self.marks.panned(x_fraction, true),
            self.marks.panned(y_fraction, false),
        ) else {
            return false;
        };
        self.set_viewport(
            Some(AxisRange::Fixed {
                min: x.min,
                max: x.max,
            }),
            Some(AxisRange::Fixed {
                min: y.min,
                max: y.max,
            }),
        )
    }
    pub fn zoom_to_selection(&mut self, selection: CartesianBrushSelection) -> bool {
        self.set_viewport(Some(selection.x), selection.y)
    }
}

impl CartesianMarks {
    fn pan_view_pixels(&mut self, dx: f64, dy: f64, plot: Rect) -> bool {
        if !dx.is_finite()
            || !dy.is_finite()
            || plot.width() <= 0.0
            || plot.height() <= 0.0
            || self.style.pan == CartesianPan::Disabled
        {
            return false;
        }
        let shift = |domain: Domain, pixels: f64, extent: f64, scale: AxisScale| {
            let min = Self::transform(domain.min, scale)?;
            let max = Self::transform(domain.max, scale)?;
            let delta = pixels / extent * (max - min);
            Some(Domain {
                min: Self::inverse(min + delta, scale),
                max: Self::inverse(max + delta, scale),
            })
        };
        let Some(x) = shift(
            self.x_view
                .unwrap_or_else(|| self.base_domain(&self.style.x_axis, true)),
            -dx,
            plot.width(),
            self.style.x_axis.scale,
        ) else {
            return false;
        };
        let y = if self.style.pan == CartesianPan::XY {
            let Some(y) = shift(
                self.y_view
                    .unwrap_or_else(|| self.base_domain(&self.style.y_axis, false)),
                dy,
                plot.height(),
                self.style.y_axis.scale,
            ) else {
                return false;
            };
            Some(y)
        } else {
            self.y_view
        };
        if self.x_view.as_ref().map(|d| (d.min, d.max)) == Some((x.min, x.max))
            && self.y_view.as_ref().map(|d| (d.min, d.max)) == y.as_ref().map(|d| (d.min, d.max))
        {
            return false;
        }
        self.x_view = Some(x);
        self.y_view = y;
        self.view_changed = true;
        self.frame.inspection.hovered = self.hit(self.frame.inspection.pointer);
        true
    }

    fn panned(&self, fraction: f64, x_axis: bool) -> Option<Domain> {
        let (view, axis) = if x_axis {
            (self.x_view, &self.style.x_axis)
        } else {
            (self.y_view, &self.style.y_axis)
        };
        if !fraction.is_finite() {
            return None;
        }
        let domain = view.unwrap_or_else(|| self.base_domain(axis, x_axis));
        let min = Self::transform(domain.min, axis.scale)?;
        let max = Self::transform(domain.max, axis.scale)?;
        let delta = (max - min) * fraction;
        Some(Domain {
            min: Self::inverse(min + delta, axis.scale),
            max: Self::inverse(max + delta, axis.scale),
        })
    }

    fn hovered_datum(&self) -> Option<(usize, usize)> {
        match self.frame.inspection.hovered? {
            CartesianHover::Datum { series, datum } => Some((series, datum)),
            CartesianHover::Annotation(_) => None,
        }
    }
    fn hovered_annotation(&self) -> Option<usize> {
        match self.frame.inspection.hovered? {
            CartesianHover::Annotation(index) => Some(index),
            CartesianHover::Datum { .. } => None,
        }
    }

    fn plot_rect(&self) -> Rect {
        let b = self.bands;
        Rect::new(
            self.frame.rect.x0 + b.left,
            self.frame.rect.y0 + b.top,
            (self.frame.rect.x1 - b.right).max(self.frame.rect.x0 + b.left + 1.0),
            (self.frame.rect.y1 - b.bottom).max(self.frame.rect.y0 + b.top + 1.0),
        )
    }

    /// Measure the bands around the plot in `theme`: the widest y tick
    /// label and the tick, title and legend lines, each a spacing step from
    /// its neighbour.
    fn measure_bands(&self, tr: &mut TextRenderer, theme: &Theme) -> Bands {
        let spacing = &theme.tokens.spacing;
        let gap = f64::from(spacing.s2);
        let tight = f64::from(spacing.s1);
        let caption = theme.text(TextRole::Caption);
        let tick_line = f64::from(tr.measure_style("0", None, &caption).1);
        let yd = self.domain(&self.style.y_axis, false);
        let widest_tick = Self::ticks(&self.style.y_axis, yd, self.frame.rect.height().max(1.0))
            .iter()
            .map(|tick| f64::from(tr.measure_style(&tick.label, None, &caption).0))
            .fold(0.0, f64::max);
        let x_title = if self.style.x_axis.title.is_empty() {
            0.0
        } else {
            tick_line + tight
        };
        let right = self.style.plot_padding.max(gap);
        let legend_rows = if self.style.show_legend {
            chart_legend::rows(
                self.series.iter().map(|series| series.name.as_str()),
                self.frame.rect.x0,
                self.frame.rect.x1 - right,
            )
        } else {
            0
        };
        let header = crate::widget::plot::Margins::header(theme, &self.frame.title, legend_rows);
        let legend_top = header.legend;
        let top = header.top + tick_line / 2.0;
        let bottom = gap + tick_line + x_title;
        // The y title runs up the plot's height, on as many lines as it takes to be read whole.
        let title_lines = Self::axis_title_lines(
            tr,
            theme,
            &self.style.y_axis.title,
            self.frame.rect.height() - top - bottom,
        )
        .len();
        let y_title = if title_lines == 0 {
            0.0
        } else {
            crate::num::f64_of_usize(title_lines) * tick_line + tight
        };
        let left = (y_title + widest_tick + gap).ceil();
        Bands {
            left,
            top: top.ceil(),
            right,
            bottom: bottom.ceil(),
            legend_top,
            gap,
            tick_line,
        }
    }

    fn domain(&self, axis: &PlotAxis, x_axis: bool) -> Domain {
        if let Some(view) = if x_axis { self.x_view } else { self.y_view } {
            return view;
        }
        self.base_domain(axis, x_axis)
    }

    fn base_domain(&self, axis: &PlotAxis, x_axis: bool) -> Domain {
        {
            let cache = self.domain_cache.borrow();
            if let Some(cache) = cache.as_ref()
                && cache.x_axis == self.style.x_axis
                && cache.y_axis == self.style.y_axis
                && let Some(domain) = if x_axis { cache.x } else { cache.y }
            {
                return domain;
            }
        }
        let domain = self.compute_base_domain(axis, x_axis);
        let mut cache = self.domain_cache.borrow_mut();
        if cache.as_ref().is_none_or(|cache| {
            cache.x_axis != self.style.x_axis || cache.y_axis != self.style.y_axis
        }) {
            *cache = Some(DomainCache {
                x_axis: self.style.x_axis.clone(),
                y_axis: self.style.y_axis.clone(),
                x: None,
                y: None,
            });
        }
        if let Some(cache) = cache.as_mut() {
            if x_axis {
                cache.x = Some(domain);
            } else {
                cache.y = Some(domain);
            }
        }
        domain
    }

    fn invalidate_domains(&self) {
        *self.domain_cache.borrow_mut() = None;
    }

    fn compute_base_domain(&self, axis: &PlotAxis, x_axis: bool) -> Domain {
        if let AxisRange::Fixed { min, max } = axis.range {
            if max > min
                && Self::transform(min, axis.scale).is_some()
                && Self::transform(max, axis.scale).is_some()
            {
                return Domain { min, max };
            }
            return if axis.scale == AxisScale::Log10 {
                Domain {
                    min: 1.0,
                    max: 10.0,
                }
            } else {
                Domain::normalized(min, max)
            };
        }
        let values = self
            .series
            .iter()
            .filter(|series| !self.toggles.is_hidden(&series.id))
            .flat_map(|series| series.data.iter())
            .map(|datum| if x_axis { datum.x } else { datum.y })
            .filter(|value| Self::transform(*value, axis.scale).is_some());
        let (mut min, mut max) = values
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), value| {
                (min.min(value), max.max(value))
            });
        if axis.include_zero && axis.scale == AxisScale::Linear {
            min = min.min(0.0);
            max = max.max(0.0);
        }
        if axis.scale == AxisScale::Log10 {
            if !min.is_finite() || !max.is_finite() {
                return Domain {
                    min: 1.0,
                    max: 10.0,
                };
            }
            if max <= min {
                return Domain {
                    min: min / 10.0_f64.sqrt(),
                    max: min * 10.0_f64.sqrt(),
                };
            }
            let log_min = min.log10();
            let log_max = max.log10();
            let padding = (log_max - log_min) * 0.06;
            return Domain {
                min: 10.0_f64.powf(log_min - padding),
                max: 10.0_f64.powf(log_max + padding),
            };
        }
        let domain = Domain::normalized(min, max);
        let padding = (domain.max - domain.min) * 0.06;
        let padded_min = if axis.include_zero && domain.min >= 0.0 {
            0.0
        } else {
            domain.min - padding
        };
        let padded_max = if axis.include_zero && domain.max <= 0.0 {
            0.0
        } else {
            domain.max + padding
        };
        Domain::normalized(padded_min, padded_max)
    }

    fn zoom_domain(
        current: Domain,
        base: Domain,
        anchor: f64,
        factor: f64,
        minimum: f64,
        scale: AxisScale,
    ) -> Domain {
        let scaled = |domain: Domain| Domain {
            min: Self::transform(domain.min, scale).unwrap_or(domain.min),
            max: Self::transform(domain.max, scale).unwrap_or(domain.max),
        };
        let current = scaled(current);
        let base = scaled(base);
        let anchor = Self::transform(anchor, scale).unwrap_or(anchor);
        let min_span = (base.max - base.min) * minimum.clamp(0.000_001, 1.0);
        let span =
            ((current.max - current.min) * factor).clamp(min_span, (base.max - base.min) * 128.0);
        let ratio = ((anchor - current.min) / (current.max - current.min)).clamp(0.0, 1.0);
        Domain {
            min: Self::inverse(anchor - span * ratio, scale),
            max: Self::inverse(anchor + span * (1.0 - ratio), scale),
        }
    }

    fn zoom_at_pointer(&mut self, delta: f64) {
        let plot = self.plot_rect();
        if !plot.contains(self.frame.inspection.pointer)
            || self.style.zoom == CartesianZoom::Disabled
        {
            return;
        }
        let factor = (-delta * 0.12).exp().clamp(0.5, 2.0);
        let base_x = self.base_domain(&self.style.x_axis, true);
        let current_x = self.x_view.unwrap_or(base_x);
        let x_anchor = Self::unproject(
            self.frame.inspection.pointer.x,
            current_x,
            self.style.x_axis.scale,
            plot.x0,
            plot.width(),
        );
        self.x_view = Some(Self::zoom_domain(
            current_x,
            base_x,
            x_anchor,
            factor,
            self.style.min_zoom_fraction,
            self.style.x_axis.scale,
        ));
        if self.style.zoom == CartesianZoom::XY {
            let base_y = self.base_domain(&self.style.y_axis, false);
            let current_y = self.y_view.unwrap_or(base_y);
            let y_anchor = Self::unproject(
                self.frame.inspection.pointer.y,
                current_y,
                self.style.y_axis.scale,
                plot.y1,
                -plot.height(),
            );
            self.y_view = Some(Self::zoom_domain(
                current_y,
                base_y,
                y_anchor,
                factor,
                self.style.min_zoom_fraction,
                self.style.y_axis.scale,
            ));
        }
        self.view_changed = true;
        self.frame.inspection.hovered = self.hit(self.frame.inspection.pointer);
    }

    fn map_xy(
        plot: Rect,
        x_domain: Domain,
        y_domain: Domain,
        x_scale: AxisScale,
        y_scale: AxisScale,
        x_value: f64,
        y_value: f64,
    ) -> Option<Point> {
        let x = Self::project(x_value, x_domain, x_scale, plot.x0, plot.width())?;
        let y = Self::project(y_value, y_domain, y_scale, plot.y1, -plot.height())?;
        Some(Point::new(x, y))
    }

    fn map_animated_datum(
        &self,
        series: usize,
        datum: usize,
        plot: Rect,
        x_domain: Domain,
        y_domain: Domain,
        now: Instant,
    ) -> Option<Point> {
        let (x, y) = self.animated_xy(series, datum, now)?;
        Self::map_xy(
            plot,
            x_domain,
            y_domain,
            self.style.x_axis.scale,
            self.style.y_axis.scale,
            x,
            y,
        )
    }

    fn project(
        value: f64,
        domain: Domain,
        scale: AxisScale,
        origin: f64,
        span: f64,
    ) -> Option<f64> {
        let value = Self::transform(value, scale)?;
        let min = Self::transform(domain.min, scale)?;
        let max = Self::transform(domain.max, scale)?;
        Some(((value - min) / (max - min)).mul_add(span, origin))
    }

    fn animation_progress_at(&self, now: Instant) -> f64 {
        if self.reduce_motion || !self.style.animation.enabled || self.transition_from.is_none() {
            return 1.0;
        }
        crate::widget::pie_chart::data_change_progress(
            now.saturating_duration_since(self.transition_started),
        )
    }

    fn animated_xy(&self, series: usize, datum: usize, now: Instant) -> Option<(f64, f64)> {
        let target_series = self.series.get(series)?;
        let target = target_series.data.get(datum)?;
        let progress = self.animation_progress_at(now);
        if progress >= 1.0 {
            return Some((target.x, target.y));
        }
        let previous = self
            .transition_from
            .as_ref()?
            .iter()
            .find(|series| series.id == target_series.id)?
            .data
            .iter()
            .find(|datum| datum.id == target.id)?;
        Some((
            previous.x + (target.x - previous.x) * progress,
            previous.y + (target.y - previous.y) * progress,
        ))
    }

    fn transform(value: f64, scale: AxisScale) -> Option<f64> {
        match scale {
            AxisScale::Linear => value.is_finite().then_some(value),
            AxisScale::Log10 => (value.is_finite() && value > 0.0).then(|| value.log10()),
        }
    }

    fn inverse(value: f64, scale: AxisScale) -> f64 {
        match scale {
            AxisScale::Linear => value,
            AxisScale::Log10 => 10.0_f64.powf(value),
        }
    }

    fn unproject(value: f64, domain: Domain, scale: AxisScale, origin: f64, span: f64) -> f64 {
        let min = Self::transform(domain.min, scale).unwrap_or(domain.min);
        let max = Self::transform(domain.max, scale).unwrap_or(domain.max);
        Self::inverse(min + (value - origin) / span * (max - min), scale)
    }

    fn brush_rect(&self) -> Option<Rect> {
        let start = self.brush_start?;
        let plot = self.plot_rect();
        let current = self.brush_current;
        let x0 = start.x.min(current.x).clamp(plot.x0, plot.x1);
        let x1 = start.x.max(current.x).clamp(plot.x0, plot.x1);
        let (y0, y1) = if self.style.brush == CartesianBrush::X {
            (plot.y0, plot.y1)
        } else {
            (
                start.y.min(current.y).clamp(plot.y0, plot.y1),
                start.y.max(current.y).clamp(plot.y0, plot.y1),
            )
        };
        Some(Rect::new(x0, y0, x1, y1))
    }

    fn brush_selection_from_rect(
        rect: Rect,
        plot: Rect,
        x_domain: Domain,
        y_domain: Domain,
        x_scale: AxisScale,
        y_scale: AxisScale,
        mode: CartesianBrush,
    ) -> Option<CartesianBrushSelection> {
        if rect.width() < 4.0 || (mode == CartesianBrush::XY && rect.height() < 4.0) {
            return None;
        }
        let x = AxisRange::Fixed {
            min: Self::unproject(rect.x0, x_domain, x_scale, plot.x0, plot.width()),
            max: Self::unproject(rect.x1, x_domain, x_scale, plot.x0, plot.width()),
        };
        let y = (mode == CartesianBrush::XY).then(|| AxisRange::Fixed {
            min: Self::unproject(rect.y1, y_domain, y_scale, plot.y1, -plot.height()),
            max: Self::unproject(rect.y0, y_domain, y_scale, plot.y1, -plot.height()),
        });
        Some(CartesianBrushSelection { x, y })
    }

    fn finish_brush(&mut self) -> bool {
        let Some(rect) = self.brush_rect() else {
            self.brush_start = None;
            return false;
        };
        self.brush_start = None;
        let plot = self.plot_rect();
        let xd = self.domain(&self.style.x_axis, true);
        let yd = self.domain(&self.style.y_axis, false);
        self.brushed = Self::brush_selection_from_rect(
            rect,
            plot,
            xd,
            yd,
            self.style.x_axis.scale,
            self.style.y_axis.scale,
            self.style.brush,
        );
        self.brushed.is_some()
    }

    fn ticks(axis: &PlotAxis, domain: Domain, span: f64) -> Vec<AxisTick> {
        if !axis.ticks.is_empty() {
            let available = crate::num::usize_of_f64((span / axis.min_tick_spacing).floor()).max(2);
            if axis.ticks.len() <= available {
                return axis.ticks.clone();
            }
            let last = axis.ticks.len().saturating_sub(1);
            return (0..available)
                .filter_map(|index| {
                    let position = index
                        .saturating_mul(last)
                        .checked_div(available.saturating_sub(1))?;
                    axis.ticks.get(position).cloned()
                })
                .collect();
        }
        let responsive_count =
            crate::num::u8_of_f64((span / axis.min_tick_spacing).floor()).saturating_add(1);
        let count = axis.tick_count.clamp(2, 20).min(responsive_count.max(2));
        if axis.scale == AxisScale::Log10 {
            let first = crate::num::i32_of_f64(domain.min.log10().ceil());
            let last = crate::num::i32_of_f64(domain.max.log10().floor());
            let decades = if last >= first {
                (first..=last)
                    .map(|power| 10.0_f64.powi(power))
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            if decades.len() >= 2 {
                let available = usize::from(count);
                let sample_count = available.min(decades.len());
                let last = decades.len().saturating_sub(1);
                return (0..sample_count)
                    .filter_map(|index| {
                        let position = index
                            .saturating_mul(last)
                            .checked_div(sample_count.saturating_sub(1))?;
                        let value = *decades.get(position)?;
                        Some(AxisTick::new(
                            value,
                            format_axis_value(value, axis.label_format),
                        ))
                    })
                    .collect();
            }
        }
        if axis.scale == AxisScale::Linear {
            // Round numbers inside the domain, not the domain cut in equal
            // parts: an axis reads 0, 10, 20, never 0, 11.5, 23.
            let values = crate::charts::nice_ticks(domain.min, domain.max, usize::from(count));
            if values.len() >= 2 {
                let step = crate::widget::plot::tick_step(&values);
                return values
                    .into_iter()
                    .map(|value| {
                        AxisTick::new(value, format_axis_tick(value, step, axis.label_format))
                    })
                    .collect();
            }
        }
        (0..count)
            .map(|index| {
                let fraction = f64::from(index) / f64::from(count - 1);
                let min = Self::transform(domain.min, axis.scale).unwrap_or(domain.min);
                let max = Self::transform(domain.max, axis.scale).unwrap_or(domain.max);
                let value = Self::inverse(min + (max - min) * fraction, axis.scale);
                let label = format_axis_value(value, axis.label_format);
                AxisTick::new(value, label)
            })
            .collect()
    }

    fn hit(&self, pointer: Point) -> Option<CartesianHover> {
        let plot = self.plot_rect();
        if !plot
            .inflate(self.style.hit_radius, self.style.hit_radius)
            .contains(pointer)
        {
            return None;
        }
        let xd = self.domain(&self.style.x_axis, true);
        let yd = self.domain(&self.style.y_axis, false);
        let now = crate::time::now();
        // A line or a band stands over the plot's reading only where the pointer is on it.
        let annotation = self
            .annotation_at(pointer, plot, xd, yd)
            .filter(|&index| {
                self.annotations.get(index).is_some_and(|annotation| {
                    !matches!(
                        annotation.kind,
                        CartesianAnnotationKind::XBand { .. }
                            | CartesianAnnotationKind::YBand { .. }
                    )
                })
            })
            .map(CartesianHover::Annotation);
        if self.style.tooltip.shared && plot.contains(pointer) {
            if annotation.is_some() {
                return annotation;
            }
            if let Some(hover) = self.nearest_x(pointer, plot, xd, yd, now) {
                return Some(hover);
            }
        }
        let mut nearest: Option<(f64, CartesianHover)> = None;
        for (series, values) in self.series.iter().enumerate() {
            if self.toggles.is_hidden(&values.id) {
                continue;
            }
            for datum in self.hover_range(values, plot, xd) {
                let Some(point) = self.map_animated_datum(series, datum, plot, xd, yd, now) else {
                    continue;
                };
                let distance = point.distance(pointer);
                if distance <= self.style.hit_radius
                    && nearest.is_none_or(|current| distance < current.0)
                {
                    nearest = Some((distance, CartesianHover::Datum { series, datum }));
                }
            }
        }
        nearest.map(|(_, hover)| hover).or_else(|| {
            self.annotation_at(pointer, plot, xd, yd)
                .map(CartesianHover::Annotation)
        })
    }

    /// Whether `series` is read with the others at an x position: a line, an area, a step or
    /// bars, not a cloud of points.
    fn reads_across(series: &PlotSeries) -> bool {
        !matches!(series.kind, SeriesKind::Scatter | SeriesKind::Bubble)
    }

    /// The datum of a series read across whose x stands nearest the pointer's, the nearer one in
    /// height where two share it.
    fn nearest_x(
        &self,
        pointer: Point,
        plot: Rect,
        xd: Domain,
        yd: Domain,
        now: Instant,
    ) -> Option<CartesianHover> {
        let mut nearest: Option<((f64, f64), CartesianHover)> = None;
        for (series, values) in self.series.iter().enumerate() {
            if self.toggles.is_hidden(&values.id) || !Self::reads_across(values) {
                continue;
            }
            let range = self.hover_range(values, plot, xd);
            let range = if range.is_empty() {
                0..values.data.len()
            } else {
                range
            };
            for datum in range {
                let Some(point) = self.map_animated_datum(series, datum, plot, xd, yd, now) else {
                    continue;
                };
                let key = ((point.x - pointer.x).abs(), (point.y - pointer.y).abs());
                if nearest.is_none_or(|(best, _)| key < best) {
                    nearest = Some((key, CartesianHover::Datum { series, datum }));
                }
            }
        }
        nearest.map(|(_, hover)| hover)
    }

    /// Each shown series read across at `x`: its place, its datum there and where it stands.
    fn read_across(
        &self,
        x: f64,
        plot: Rect,
        xd: Domain,
        yd: Domain,
    ) -> Vec<(usize, usize, Point)> {
        let now = crate::time::now();
        self.series
            .iter()
            .enumerate()
            .filter(|(_, values)| !self.toggles.is_hidden(&values.id) && Self::reads_across(values))
            .filter_map(|(series, values)| {
                let datum = values
                    .data
                    .iter()
                    .position(|datum| (datum.x - x).abs() <= f64::EPSILON * x.abs().max(1.0))?;
                let point = self.map_animated_datum(series, datum, plot, xd, yd, now)?;
                Some((series, datum, point))
            })
            .collect()
    }

    fn hover_range(
        &self,
        series: &PlotSeries,
        plot: Rect,
        x_domain: Domain,
    ) -> std::ops::Range<usize> {
        let count = series.data.len();
        if !series.x_sorted || count < 32 || self.is_animating() || plot.width() <= 0.0 {
            return 0..count;
        }
        let a = Self::unproject(
            self.frame.inspection.pointer.x - self.style.hit_radius,
            x_domain,
            self.style.x_axis.scale,
            plot.x0,
            plot.width(),
        );
        let b = Self::unproject(
            self.frame.inspection.pointer.x + self.style.hit_radius,
            x_domain,
            self.style.x_axis.scale,
            plot.x0,
            plot.width(),
        );
        let low = a.min(b);
        let high = a.max(b);
        let start = series
            .data
            .partition_point(|datum| datum.x < low)
            .saturating_sub(1);
        let end = series
            .data
            .partition_point(|datum| datum.x <= high)
            .saturating_add(1)
            .min(count);
        start..end.max(start)
    }

    fn annotation_at(&self, pointer: Point, plot: Rect, xd: Domain, yd: Domain) -> Option<usize> {
        if !plot.contains(pointer) {
            return None;
        }
        self.annotations
            .iter()
            .enumerate()
            .filter(|(_, annotation)| annotation.interactive)
            .filter_map(|(index, annotation)| {
                let distance = match annotation.kind {
                    CartesianAnnotationKind::XLine(value) => {
                        Self::project(value, xd, self.style.x_axis.scale, plot.x0, plot.width())
                            .map(|x| (pointer.x - x).abs())?
                    }
                    CartesianAnnotationKind::YLine(value) => {
                        Self::project(value, yd, self.style.y_axis.scale, plot.y1, -plot.height())
                            .map(|y| (pointer.y - y).abs())?
                    }
                    CartesianAnnotationKind::XBand { start, end } => {
                        let a = Self::project(
                            start,
                            xd,
                            self.style.x_axis.scale,
                            plot.x0,
                            plot.width(),
                        )?;
                        let b =
                            Self::project(end, xd, self.style.x_axis.scale, plot.x0, plot.width())?;
                        if pointer.x >= a.min(b) && pointer.x <= a.max(b) {
                            5.0
                        } else {
                            f64::INFINITY
                        }
                    }
                    CartesianAnnotationKind::YBand { start, end } => {
                        let a = Self::project(
                            start,
                            yd,
                            self.style.y_axis.scale,
                            plot.y1,
                            -plot.height(),
                        )?;
                        let b = Self::project(
                            end,
                            yd,
                            self.style.y_axis.scale,
                            plot.y1,
                            -plot.height(),
                        )?;
                        if pointer.y >= a.min(b) && pointer.y <= a.max(b) {
                            5.0
                        } else {
                            f64::INFINITY
                        }
                    }
                    CartesianAnnotationKind::Point { x, y } => {
                        let x =
                            Self::project(x, xd, self.style.x_axis.scale, plot.x0, plot.width())?;
                        let y =
                            Self::project(y, yd, self.style.y_axis.scale, plot.y1, -plot.height())?;
                        pointer.distance(Point::new(x, y))
                    }
                };
                (distance <= 10.0).then_some((index, distance))
            })
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(index, _)| index)
    }

    fn annotation_anchor(&self, index: usize, plot: Rect, xd: Domain, yd: Domain) -> Option<Point> {
        let annotation = self.annotations.get(index)?;
        match annotation.kind {
            CartesianAnnotationKind::XLine(value) => {
                Self::project(value, xd, self.style.x_axis.scale, plot.x0, plot.width())
                    .map(|x| Point::new(x, plot.y0.midpoint(plot.y1)))
            }
            CartesianAnnotationKind::YLine(value) => {
                Self::project(value, yd, self.style.y_axis.scale, plot.y1, -plot.height())
                    .map(|y| Point::new(plot.x0.midpoint(plot.x1), y))
            }
            CartesianAnnotationKind::XBand { start, end } => {
                let a = Self::project(start, xd, self.style.x_axis.scale, plot.x0, plot.width())?;
                let b = Self::project(end, xd, self.style.x_axis.scale, plot.x0, plot.width())?;
                Some(Point::new(a.midpoint(b), plot.y0.midpoint(plot.y1)))
            }
            CartesianAnnotationKind::YBand { start, end } => {
                let a = Self::project(start, yd, self.style.y_axis.scale, plot.y1, -plot.height())?;
                let b = Self::project(end, yd, self.style.y_axis.scale, plot.y1, -plot.height())?;
                Some(Point::new(plot.x0.midpoint(plot.x1), a.midpoint(b)))
            }
            CartesianAnnotationKind::Point { x, y } => Some(Point::new(
                Self::project(x, xd, self.style.x_axis.scale, plot.x0, plot.width())?,
                Self::project(y, yd, self.style.y_axis.scale, plot.y1, -plot.height())?,
            )),
        }
        .filter(|point| plot.contains(*point))
    }

    fn draw_axes(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        plot: Rect,
        xd: Domain,
        yd: Domain,
    ) {
        let grid = with_alpha(theme.tokens.colors.border_standard, 100);
        let size = theme.text(TextRole::Caption);
        for tick in Self::ticks(&self.style.y_axis, yd, plot.height()) {
            let Some(y) = Self::project(
                tick.value,
                yd,
                self.style.y_axis.scale,
                plot.y1,
                -plot.height(),
            ) else {
                continue;
            };
            if self.style.y_axis.grid {
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    grid,
                    None,
                    &Line::new((plot.x0, y), (plot.x1, y)),
                );
            }
            let (width, height) = tr.measure_style(&tick.label, None, &size);
            tr.draw_style(
                scene,
                &tick.label,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((
                        plot.x0 - f64::from(width) - self.bands.gap,
                        y - f64::from(height) / 2.0,
                    )),
                None,
                &size,
            );
        }
        for tick in Self::ticks(&self.style.x_axis, xd, plot.width()) {
            let Some(x) = Self::project(
                tick.value,
                xd,
                self.style.x_axis.scale,
                plot.x0,
                plot.width(),
            ) else {
                continue;
            };
            if self.style.x_axis.grid {
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    grid,
                    None,
                    &Line::new((x, plot.y0), (x, plot.y1)),
                );
            }
            let (width, _) = tr.measure_style(&tick.label, None, &size);
            tr.draw_style(
                scene,
                &tick.label,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((x - f64::from(width) / 2.0, plot.y1 + self.bands.gap)),
                None,
                &size,
            );
        }
        let title_style = theme
            .text(TextRole::Caption)
            .with_weight(TextWeight::Semibold);
        if !self.style.x_axis.title.is_empty() {
            let title = tr.elide_to_width_style(
                &self.style.x_axis.title,
                crate::num::f32_of_f64(plot.width().max(0.0)),
                &title_style,
            );
            let (width, _) = tr.measure_style(&title, None, &title_style);
            tr.draw_style(
                scene,
                &title,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((
                        plot.center().x - f64::from(width) / 2.0,
                        plot.y1 + self.bands.gap + self.bands.tick_line + self.bands.gap / 2.0,
                    )),
                None,
                &title_style,
            );
        }
        let lines = Self::axis_title_lines(tr, theme, &self.style.y_axis.title, plot.height());
        for (index, line) in lines.iter().enumerate() {
            let (width, _) = tr.measure_style(line, None, &title_style);
            tr.draw_style(
                scene,
                line,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((
                        self.frame.rect.x0 + crate::num::f64_of_usize(index) * self.bands.tick_line,
                        plot.center().y + f64::from(width) / 2.0,
                    ))
                    * Affine::rotate(-std::f64::consts::FRAC_PI_2),
                None,
                &title_style,
            );
        }
    }

    /// The most lines an axis title takes before what is left of it is elided.
    const AXIS_TITLE_LINES: usize = 2;

    /// `title` broken between words into lines no longer than `available`, as measured in the
    /// axis title's type: one line where it fits, a second where it does not, and the rest of a
    /// title too long for both elided at the end of the second.
    fn axis_title_lines(
        tr: &mut TextRenderer,
        theme: &Theme,
        title: &str,
        available: f64,
    ) -> Vec<String> {
        let style = theme
            .text(TextRole::Caption)
            .with_weight(TextWeight::Semibold);
        let width = crate::num::f32_of_f64(available.max(0.0));
        let fits =
            |tr: &mut TextRenderer, text: &str| tr.measure_style(text, None, &style).0 <= width;
        let mut lines: Vec<String> = Vec::new();
        let mut words = title.split_whitespace().peekable();
        while words.peek().is_some() {
            if lines.len() + 1 == Self::AXIS_TITLE_LINES {
                let rest = words.collect::<Vec<_>>().join(" ");
                lines.push(tr.elide_to_width_style(&rest, width, &style));
                break;
            }
            let mut line = String::new();
            while let Some(word) = words.peek() {
                let longer = if line.is_empty() {
                    (*word).to_owned()
                } else {
                    format!("{line} {word}")
                };
                if !line.is_empty() && !fits(tr, &longer) {
                    break;
                }
                line = longer;
                words.next();
            }
            lines.push(if fits(tr, &line) {
                line
            } else {
                tr.elide_to_width_style(&line, width, &style)
            });
        }
        lines.retain(|line| !line.is_empty());
        lines
    }

    fn series_path(points: &[Point], kind: SeriesKind) -> BezPath {
        let mut path = BezPath::new();
        let Some(first) = points.first() else {
            return path;
        };
        path.move_to(*first);
        let last = points.len().saturating_sub(2);
        for (index, pair) in points.windows(2).enumerate() {
            let Some(previous) = pair.first() else {
                continue;
            };
            let Some(next) = pair.get(1) else {
                continue;
            };
            match kind {
                // A step holds each value until the next reading. The last reading holds past
                // the axis's end, so it has no tread here and gets no riser: a rise standing
                // alone at the edge would read as a spike.
                SeriesKind::Step | SeriesKind::StepArea => {
                    path.line_to((next.x, previous.y));
                    if index < last {
                        path.line_to(*next);
                    }
                }
                SeriesKind::Smooth | SeriesKind::SmoothArea => {
                    let midpoint = previous.x.midpoint(next.x);
                    path.curve_to((midpoint, previous.y), (midpoint, next.y), (next.x, next.y));
                }
                _ => path.line_to(*next),
            }
        }
        path
    }

    fn series_stroke(series: &PlotSeries) -> Stroke {
        let stroke = Stroke::new(series.stroke_width);
        if series.line_dash.is_empty() {
            stroke
        } else {
            stroke.with_dashes(0.0, series.line_dash.iter().copied())
        }
    }

    fn draw_marker(
        scene: &mut Scene,
        transform: Affine,
        color: Color,
        point: Point,
        radius: f64,
        shape: MarkerShape,
    ) {
        match shape {
            MarkerShape::Circle => scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &Circle::new(point, radius),
            ),
            MarkerShape::Square => scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &RoundedRect::from_rect(
                    Rect::new(
                        point.x - radius,
                        point.y - radius,
                        point.x + radius,
                        point.y + radius,
                    ),
                    (radius * 0.3).min(3.0),
                ),
            ),
            MarkerShape::Diamond => {
                let mut path = BezPath::new();
                path.move_to((point.x, point.y - radius));
                path.line_to((point.x + radius, point.y));
                path.line_to((point.x, point.y + radius));
                path.line_to((point.x - radius, point.y));
                path.close_path();
                scene.fill(Fill::NonZero, transform, color, None, &path);
            }
        }
    }

    fn point_segments(
        points: impl IntoIterator<Item = Option<Point>>,
        connect: bool,
    ) -> Vec<Vec<Point>> {
        let mut segments = Vec::new();
        let mut segment = Vec::new();
        for point in points {
            if let Some(point) = point {
                segment.push(point);
            } else if !connect && !segment.is_empty() {
                segments.push(std::mem::take(&mut segment));
            }
        }
        if !segment.is_empty() {
            segments.push(segment);
        }
        segments
    }

    fn y_baseline(&self, plot: Rect, domain: Domain) -> f64 {
        if self.style.y_axis.scale == AxisScale::Linear && domain.min <= 0.0 && domain.max >= 0.0 {
            Self::project(0.0, domain, AxisScale::Linear, plot.y1, -plot.height())
                .unwrap_or(plot.y1)
        } else {
            plot.y1
        }
    }

    /// Each reading of `series` the plot draws, with the point it stands at now, in the order
    /// it draws them: `None` for a reading with nothing to stand at.
    fn projected<'a>(
        &self,
        series_index: usize,
        series: &'a PlotSeries,
        plot: Rect,
        xd: Domain,
        yd: Domain,
        now: Instant,
    ) -> Vec<(usize, &'a PlotDatum, Option<Point>)> {
        self.render_indices(series, plot)
            .into_iter()
            .filter_map(|index| {
                Some((
                    index,
                    series.data.get(index)?,
                    self.map_animated_datum(series_index, index, plot, xd, yd, now),
                ))
            })
            .collect()
    }

    /// The share of a bar's slot left open when a narrow plot fits its bars to the slots: a
    /// twentieth, the gap between the bars of a histogram drawn wide.
    const BAR_GAP_FRACTION: f64 = 0.05;

    /// The rects a bar series fills: each bar at most `width` logical pixels wide, on whole
    /// device pixels, and never touching its neighbour. A plot too narrow to draw the bars at
    /// `width` fits them to their slots instead, keeping a gap of a twentieth of a slot and
    /// never less than one device pixel — without it the bars of a histogram run together
    /// into one silhouette and its bins can no longer be told apart. Slots under two device
    /// pixels apart leave no gap to draw at all, and no bars are drawn.
    fn bar_rects(points: &[Point], width: f64, zero: f64) -> Vec<Rect> {
        let scale = f64::from(crate::widget::device_scale()).max(f64::MIN_POSITIVE);
        let mut width_px = (width * scale).floor();
        if let Some(pitch_px) = Self::bar_pitch_px(points, scale) {
            let gap_px = (pitch_px * Self::BAR_GAP_FRACTION).ceil().max(1.0);
            // A device pixel of every slot stays unclaimed: snapping a bar's edge onto the
            // grid can move it by up to half a pixel while its neighbour moves the other way.
            width_px = width_px.min((pitch_px - gap_px).floor() - 1.0);
        }
        if width_px < 1.0 {
            return Vec::new();
        }
        let width = width_px / scale;
        points
            .iter()
            .map(|point| {
                let left = crate::widget::layout::snap_to_device_pixels(point.x - width / 2.0);
                Rect::new(left, point.y.min(zero), left + width, point.y.max(zero))
            })
            .collect()
    }

    /// The closest two bars of `points` stand, in device pixels: the slot each one has to fit
    /// in. `None` where there is no pair to keep apart.
    fn bar_pitch_px(points: &[Point], scale: f64) -> Option<f64> {
        let mut xs = points.iter().map(|point| point.x).collect::<Vec<_>>();
        xs.sort_by(f64::total_cmp);
        xs.windows(2)
            .map(|pair| (pair[1] - pair[0]) * scale)
            .filter(|pitch| *pitch > 0.0)
            .reduce(f64::min)
    }

    /// The rects the bars of series `id` cover as the plot draws them now.
    #[cfg(test)]
    fn bars_of(&self, id: &str) -> Vec<Rect> {
        let plot = self.plot_rect();
        let (index, series) = self
            .series
            .iter()
            .enumerate()
            .find(|(_, series)| series.id == id)
            .unwrap_or_else(|| panic!("the plot has no series {id}"));
        let SeriesKind::Bar { width } = series.kind else {
            panic!("{id}'s readings are not drawn as bars");
        };
        if self.toggles.is_hidden(id) {
            return Vec::new();
        }
        let xd = self.domain(&self.style.x_axis, true);
        let yd = self.domain(&self.style.y_axis, false);
        let points = self
            .projected(index, series, plot, xd, yd, crate::time::now())
            .into_iter()
            .filter_map(|(_, _, point)| point)
            .collect::<Vec<_>>();
        Self::bar_rects(&points, width, self.y_baseline(plot, yd))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "all series kinds share projection, selection and ordering in one paint pass"
    )]
    fn draw_series(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        plot: Rect,
        xd: Domain,
        yd: Domain,
        now: Instant,
    ) {
        for (series_index, series) in self.series.iter().enumerate() {
            if self.toggles.is_hidden(&series.id) {
                continue;
            }
            let color = series.color_in(theme);
            let projected = self.projected(series_index, series, plot, xd, yd, now);
            let points = projected
                .iter()
                .filter_map(|(_, _, point)| *point)
                .collect::<Vec<_>>();
            let segments = Self::point_segments(
                projected.iter().map(|(_, _, point)| *point),
                series.connect_gaps,
            );
            match series.kind {
                SeriesKind::Line | SeriesKind::Smooth | SeriesKind::Step => {
                    for segment in &segments {
                        scene.stroke(
                            &Self::series_stroke(series),
                            transform,
                            color,
                            None,
                            &Self::series_path(segment, series.kind),
                        );
                    }
                }
                SeriesKind::Area | SeriesKind::SmoothArea | SeriesKind::StepArea => {
                    for segment in &segments {
                        let (Some(first), Some(last)) = (segment.first(), segment.last()) else {
                            continue;
                        };
                        let baseline = self.y_baseline(plot, yd);
                        let path_kind = match series.kind {
                            SeriesKind::SmoothArea => SeriesKind::Smooth,
                            SeriesKind::StepArea => SeriesKind::Step,
                            _ => SeriesKind::Line,
                        };
                        let mut area = Self::series_path(segment, path_kind);
                        area.line_to((last.x, baseline));
                        area.line_to((first.x, baseline));
                        area.close_path();
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            with_alpha(color, series.fill_alpha),
                            None,
                            &area,
                        );
                        scene.stroke(
                            &Self::series_stroke(series),
                            transform,
                            color,
                            None,
                            &Self::series_path(segment, path_kind),
                        );
                    }
                }
                SeriesKind::Bar { width } => {
                    let zero = self.y_baseline(plot, yd);
                    for rect in Self::bar_rects(&points, width, zero) {
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            color,
                            None,
                            &RoundedRect::from_rect(rect, series.bar_radius),
                        );
                    }
                }
                SeriesKind::Lollipop => {
                    let zero = self.y_baseline(plot, yd);
                    for point in &points {
                        scene.stroke(
                            &Stroke::new(series.stroke_width),
                            transform,
                            color,
                            None,
                            &Line::new((point.x, zero), *point),
                        );
                        Self::draw_marker(
                            scene,
                            transform,
                            color,
                            *point,
                            series.marker_radius,
                            series.marker_shape,
                        );
                    }
                }
                SeriesKind::Scatter | SeriesKind::Bubble => {}
            }
            let markers = series.show_markers.unwrap_or(
                matches!(series.kind, SeriesKind::Scatter | SeriesKind::Bubble)
                    || self.style.rendering.show_markers_on_lines,
            );
            if markers {
                for &(datum_index, datum, point) in &projected {
                    let Some(point) = point else {
                        continue;
                    };
                    let radius = if series.kind == SeriesKind::Bubble {
                        datum.radius
                    } else {
                        series.marker_radius
                    };
                    let selected = self.hovered_datum() == Some((series_index, datum_index));
                    if selected {
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            ChartPalette::area_of(color),
                            None,
                            &Circle::new(point, radius + f64::from(theme.tokens.spacing.s2)),
                        );
                    }
                    Self::draw_marker(scene, transform, color, point, radius, series.marker_shape);
                }
            }
        }
    }

    fn render_indices(&self, series: &PlotSeries, plot: Rect) -> Vec<usize> {
        let connected = matches!(
            series.kind,
            SeriesKind::Line
                | SeriesKind::Smooth
                | SeriesKind::Step
                | SeriesKind::Area
                | SeriesKind::SmoothArea
                | SeriesKind::StepArea
        );
        let count = series.data.len();
        let points_per_pixel = self.style.rendering.max_points_per_pixel.clamp(0.25, 16.0);
        let target = crate::num::usize_of_f64((plot.width().max(1.0) * points_per_pixel).ceil());
        let target = target.max(16);
        if !self.style.rendering.adaptive_sampling || !connected || count <= target || count <= 2 {
            return (0..count).collect();
        }

        // Two extrema per contiguous bucket preserve narrow peaks and troughs
        // that stride-based sampling would silently erase.
        let buckets = target.saturating_sub(2).checked_div(2).unwrap_or(1).max(1);
        let interior = count - 2;
        let bucket_size = interior.div_ceil(buckets);
        let mut indices = Vec::with_capacity(target);
        indices.push(0);
        for start in (1..count - 1).step_by(bucket_size) {
            let end = (start + bucket_size).min(count - 1);
            let mut min = start;
            let mut max = start;
            let Some(bucket) = series.data.get(start..end) else {
                continue;
            };
            let mut min_y = bucket.first().map_or(0.0, |datum| datum.y);
            let mut max_y = min_y;
            for (offset, datum) in bucket.iter().enumerate().skip(1) {
                let index = start.saturating_add(offset);
                if datum.y < min_y {
                    min = index;
                    min_y = datum.y;
                }
                if datum.y > max_y {
                    max = index;
                    max_y = datum.y;
                }
            }
            if min <= max {
                indices.push(min);
                if max != min {
                    indices.push(max);
                }
            } else {
                indices.push(max);
                indices.push(min);
            }
        }
        indices.push(count - 1);
        if !series.connect_gaps {
            indices.extend(
                series
                    .data
                    .iter()
                    .enumerate()
                    .filter(|(_, datum)| {
                        Self::transform(datum.x, self.style.x_axis.scale).is_none()
                            || Self::transform(datum.y, self.style.y_axis.scale).is_none()
                    })
                    .map(|(index, _)| index),
            );
            indices.compact_sort();
            indices.dedup();
        }
        indices
    }

    fn annotation_stroke(annotation: &CartesianAnnotation, hovered: bool) -> Stroke {
        let stroke = Stroke::new(annotation.stroke_width + if hovered { 1.5 } else { 0.0 });
        if annotation.dashed {
            stroke.with_dashes(0.0, [6.0, 5.0])
        } else {
            stroke
        }
    }

    fn draw_annotations(&self, context: &mut AnnotationPaintContext<'_>) {
        for (index, annotation) in self.annotations.iter().enumerate() {
            let hovered = self.hovered_annotation() == Some(index);
            let anchor = self.draw_annotation_shape(context, annotation, hovered);
            Self::draw_annotation_label(context, annotation, anchor);
        }
    }

    fn draw_annotation_shape(
        &self,
        context: &mut AnnotationPaintContext<'_>,
        annotation: &CartesianAnnotation,
        hovered: bool,
    ) -> Option<Point> {
        match annotation.kind {
            CartesianAnnotationKind::XLine(value) => {
                self.draw_annotation_line(context, annotation, hovered, value, true)
            }
            CartesianAnnotationKind::YLine(value) => {
                self.draw_annotation_line(context, annotation, hovered, value, false)
            }
            CartesianAnnotationKind::XBand { start, end } => {
                self.draw_annotation_band(context, annotation, hovered, (start, end), true)
            }
            CartesianAnnotationKind::YBand { start, end } => {
                self.draw_annotation_band(context, annotation, hovered, (start, end), false)
            }
            CartesianAnnotationKind::Point { x, y } => {
                self.draw_annotation_point(context, annotation, hovered, x, y)
            }
        }
    }

    fn draw_annotation_line(
        &self,
        context: &mut AnnotationPaintContext<'_>,
        annotation: &CartesianAnnotation,
        hovered: bool,
        value: f64,
        vertical: bool,
    ) -> Option<Point> {
        let plot = context.plot;
        let (domain, scale, origin, extent) = if vertical {
            (
                context.x_domain,
                self.style.x_axis.scale,
                plot.x0,
                plot.width(),
            )
        } else {
            (
                context.y_domain,
                self.style.y_axis.scale,
                plot.y1,
                -plot.height(),
            )
        };
        let coordinate = Self::project(value, domain, scale, origin, extent)?;
        let within_plot = if vertical {
            coordinate >= plot.x0 && coordinate <= plot.x1
        } else {
            coordinate >= plot.y0 && coordinate <= plot.y1
        };
        if !within_plot {
            return None;
        }
        let line = if vertical {
            Line::new((coordinate, plot.y0), (coordinate, plot.y1))
        } else {
            Line::new((plot.x0, coordinate), (plot.x1, coordinate))
        };
        context.scene.stroke(
            &Self::annotation_stroke(annotation, hovered),
            context.transform,
            annotation.color,
            None,
            &line,
        );
        Some(if vertical {
            Point::new(coordinate + 6.0, plot.y0 + 6.0)
        } else {
            Point::new(plot.x0 + 6.0, coordinate - 26.0)
        })
    }

    fn draw_annotation_band(
        &self,
        context: &mut AnnotationPaintContext<'_>,
        annotation: &CartesianAnnotation,
        hovered: bool,
        range: (f64, f64),
        vertical: bool,
    ) -> Option<Point> {
        let plot = context.plot;
        let (domain, scale, origin, extent, lower, upper) = if vertical {
            (
                context.x_domain,
                self.style.x_axis.scale,
                plot.x0,
                plot.width(),
                plot.x0,
                plot.x1,
            )
        } else {
            (
                context.y_domain,
                self.style.y_axis.scale,
                plot.y1,
                -plot.height(),
                plot.y0,
                plot.y1,
            )
        };
        let a = Self::project(range.0, domain, scale, origin, extent)?;
        let b = Self::project(range.1, domain, scale, origin, extent)?;
        let start = a.min(b).clamp(lower, upper);
        let end = a.max(b).clamp(lower, upper);
        if end <= start {
            return None;
        }
        let rect = if vertical {
            Rect::new(start, plot.y0, end, plot.y1)
        } else {
            Rect::new(plot.x0, start, plot.x1, end)
        };
        let fill_alpha = annotation
            .fill_alpha
            .saturating_add(if hovered { 28 } else { 0 });
        context.scene.fill(
            Fill::NonZero,
            context.transform,
            with_alpha(annotation.color, fill_alpha),
            None,
            &rect,
        );
        Some(if vertical {
            Point::new(start + 6.0, plot.y0 + 6.0)
        } else {
            Point::new(plot.x0 + 6.0, start + 6.0)
        })
    }

    fn draw_annotation_point(
        &self,
        context: &mut AnnotationPaintContext<'_>,
        annotation: &CartesianAnnotation,
        hovered: bool,
        x: f64,
        y: f64,
    ) -> Option<Point> {
        let plot = context.plot;
        let x = Self::project(
            x,
            context.x_domain,
            self.style.x_axis.scale,
            plot.x0,
            plot.width(),
        )?;
        let y = Self::project(
            y,
            context.y_domain,
            self.style.y_axis.scale,
            plot.y1,
            -plot.height(),
        )?;
        if !plot.contains(Point::new(x, y)) {
            return None;
        }
        context.scene.fill(
            Fill::NonZero,
            context.transform,
            with_alpha(annotation.color, 55),
            None,
            &Circle::new((x, y), if hovered { 12.0 } else { 9.0 }),
        );
        context.scene.fill(
            Fill::NonZero,
            context.transform,
            annotation.color,
            None,
            &Circle::new((x, y), 4.0),
        );
        Some(Point::new(x + 8.0, y - 28.0))
    }

    fn draw_annotation_label(
        context: &mut AnnotationPaintContext<'_>,
        annotation: &CartesianAnnotation,
        anchor: Option<Point>,
    ) {
        let Some(anchor) = anchor.filter(|_| !annotation.label.is_empty()) else {
            return;
        };
        let style = context.theme.text(TextRole::Caption);
        let (width, height) = context.text.measure_style(&annotation.label, None, &style);
        let width = f64::from(width) + 16.0;
        let height = f64::from(height) + 10.0;
        let plot = context.plot;
        let x = anchor
            .x
            .clamp(plot.x0 + 4.0, (plot.x1 - width - 4.0).max(plot.x0 + 4.0));
        let y = anchor
            .y
            .clamp(plot.y0 + 4.0, (plot.y1 - height - 4.0).max(plot.y0 + 4.0));
        let rect = RoundedRect::from_rect(Rect::new(x, y, x + width, y + height), 5.0);
        context.scene.fill(
            Fill::NonZero,
            context.transform,
            context.theme.tokens.colors.surfaces.overlay,
            None,
            &rect,
        );
        context.scene.stroke(
            &Stroke::new(1.0),
            context.transform,
            with_alpha(annotation.color, 160),
            None,
            &rect,
        );
        context.text.draw_style(
            context.scene,
            &annotation.label,
            context.theme.tokens.colors.text_primary,
            context.transform * Affine::translate((x + 8.0, y + 5.0)),
            None,
            &style,
        );
    }

    fn legend_area(&self) -> Option<Rect> {
        if !self.style.show_legend {
            return None;
        }
        let y = self.frame.rect.y0 + self.bands.legend_top;
        Some(Rect::new(
            self.frame.rect.x0,
            y,
            self.frame.rect.x1 - self.bands.right,
            y,
        ))
    }

    fn place_legend(&mut self) {
        self.toggles.place(
            self.series
                .iter()
                .map(|series| (series.id.as_str(), series.name.as_str())),
            self.legend_area(),
        );
    }

    fn draw_legend(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
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

    /// What the plot reads at the inspected datum or annotation.
    fn readout_of(&self, theme: &Theme) -> Option<Readout> {
        let hover = self
            .frame
            .inspection
            .hovered
            .filter(|_| self.style.tooltip.enabled)?;
        let pointer = self.frame.inspection.pointer;
        let (x_format, y_format) = (
            self.style.x_axis.label_format,
            self.style.y_axis.label_format,
        );
        match hover {
            CartesianHover::Datum { series, datum } => {
                let values = self.series.get(series)?;
                let datum = values.data.get(datum)?;
                let point = self.anchor(&hover)?;
                let anchor = match self.style.tooltip.anchor {
                    CartesianTooltipAnchor::Pointer if self.frame.rect.contains(pointer) => pointer,
                    CartesianTooltipAnchor::Pointer | CartesianTooltipAnchor::Datum => point,
                };
                let plot = self.plot_rect();
                if self.style.tooltip.shared && Self::reads_across(values) {
                    let title = if datum.label.is_empty() {
                        format_axis_value(datum.x, x_format)
                    } else {
                        datum.label.clone()
                    };
                    let mut readout = Readout::new(
                        title,
                        Point::new(point.x, anchor.y.clamp(plot.y0, plot.y1)),
                        self.frame.rect,
                    );
                    if self.style.rendering.show_crosshair {
                        readout.mark(Highlight::Guide {
                            x: point.x,
                            y0: plot.y0,
                            y1: plot.y1,
                        });
                    }
                    let xd = self.domain(&self.style.x_axis, true);
                    let yd = self.domain(&self.style.y_axis, false);
                    for (line, index, at) in self.read_across(datum.x, plot, xd, yd) {
                        let (Some(read), Some(reading)) = (
                            self.series.get(line),
                            self.series.get(line).and_then(|s| s.data.get(index)),
                        ) else {
                            continue;
                        };
                        let value = if reading.detail.is_empty() {
                            format_axis_value(reading.y, y_format)
                        } else {
                            reading.detail.clone()
                        };
                        readout
                            .mark(Highlight::Dot {
                                at,
                                color: read.color_in(theme),
                            })
                            .value(
                                Some(read.color_in(theme)),
                                read.name.clone(),
                                value,
                                line == series,
                            );
                    }
                    // A band the pointer is in is named under the values it holds.
                    if let Some(band) = self
                        .annotation_at(pointer, plot, xd, yd)
                        .and_then(|index| self.annotations.get(index))
                        .filter(|annotation| {
                            matches!(
                                annotation.kind,
                                CartesianAnnotationKind::XBand { .. }
                                    | CartesianAnnotationKind::YBand { .. }
                            )
                        })
                    {
                        readout.note(if band.label.is_empty() {
                            band.id.clone()
                        } else {
                            band.label.clone()
                        });
                    }
                    return Some(readout);
                }
                let title = if datum.label.is_empty() {
                    values.name.clone()
                } else {
                    datum.label.clone()
                };
                let mut readout = Readout::new(title, anchor, self.frame.rect);
                readout.mark(Highlight::Dot {
                    at: point,
                    color: values.color_in(theme),
                });
                if datum.detail.is_empty() || self.style.tooltip.show_coordinates {
                    readout.value(
                        Some(values.color_in(theme)),
                        format_axis_value(datum.x, x_format),
                        format_axis_value(datum.y, y_format),
                        false,
                    );
                }
                readout.note(datum.detail.clone());
                Some(readout)
            }
            CartesianHover::Annotation(index) => {
                let annotation = self.annotations.get(index)?;
                let title = if annotation.label.is_empty() {
                    annotation.id.clone()
                } else {
                    annotation.label.clone()
                };
                let detail = if annotation.detail.is_empty() {
                    "Chart annotation".to_owned()
                } else {
                    annotation.detail.clone()
                };
                Some(Readout::of_mark(&title, &detail, pointer, self.frame.rect))
            }
        }
    }

    fn begin_pointer_gesture(&mut self, modifiers: winit::event::Modifiers) -> bool {
        if !self.plot_rect().contains(self.frame.inspection.pointer) {
            return false;
        }
        if self.style.pan != CartesianPan::Disabled
            && (!self.style.gestures.pan_modifier_required || modifiers.state().alt_key())
        {
            self.pan_last = Some(self.frame.inspection.pointer);
        } else if self.style.brush != CartesianBrush::Disabled
            && (!self.style.gestures.brush_modifier_required || modifiers.state().shift_key())
        {
            self.brush_start = Some(self.frame.inspection.pointer);
            self.brush_current = self.frame.inspection.pointer;
        } else {
            return false;
        }
        self.frame.inspection.clear();
        true
    }

    fn finish_pointer_gesture(&mut self) -> bool {
        if self.pan_last.take().is_some() {
            return true;
        }
        if self.brush_start.is_some() {
            self.finish_brush();
            return true;
        }
        false
    }

    fn drag_to(&mut self, point: Point) -> bool {
        self.frame.inspection.pointer = point;
        if let Some(previous) = self.pan_last.replace(point) {
            self.pan_view_pixels(point.x - previous.x, point.y - previous.y, self.plot_rect());
        } else if self.brush_start.is_some() {
            self.brush_current = point;
        } else {
            return false;
        }
        self.frame.inspection.clear();
        true
    }
}

/// `value` as an axis tick `step` from the next says it: a format that picks its own precision
/// (`Auto`, and `Compact` under a thousand) writes as many decimals as the step needs, so ticks
/// a whole number apart read `0, 2, 4` and never `0.0, 2.0`; every other format is its own.
pub(crate) fn format_axis_tick(value: f64, step: f64, format: AxisLabelFormat) -> String {
    match format {
        AxisLabelFormat::Auto | AxisLabelFormat::Compact
            if value.abs() < 1000.0 && step.is_finite() && step > 0.0 =>
        {
            crate::charts::format_tick(value, step)
        }
        _ => format_axis_value(value, format),
    }
}

pub(crate) fn format_axis_value(value: f64, format: AxisLabelFormat) -> String {
    match format {
        AxisLabelFormat::Auto => {
            if value.abs() >= 1000.0 {
                format_compact(value)
            } else if value.abs() >= 10.0 {
                format!("{value:.0}")
            } else {
                format!("{value:.1}")
            }
        }
        AxisLabelFormat::Decimal(places) => {
            format!("{value:.places$}", places = usize::from(places.min(8)))
        }
        AxisLabelFormat::Compact => format_compact(value),
        AxisLabelFormat::Percent(places) => format!(
            "{percent:.places$}%",
            percent = value * 100.0,
            places = usize::from(places.min(8))
        ),
        AxisLabelFormat::Bytes(places) => format_bytes(value, places),
        AxisLabelFormat::Size => format_size(value),
        AxisLabelFormat::Scientific(places) => {
            format!("{value:.places$e}", places = usize::from(places.min(8)))
        }
        AxisLabelFormat::Duration(places) => format_duration(value, places),
        AxisLabelFormat::Currency { symbol, places } => {
            let sign = if value.is_sign_negative() { "-" } else { "" };
            format!(
                "{sign}{symbol}{absolute:.places$}",
                absolute = value.abs(),
                places = usize::from(places.min(8))
            )
        }
    }
}

fn format_duration(seconds: f64, places: u8) -> String {
    let absolute = seconds.abs();
    let (scaled, unit) = if absolute >= 86_400.0 {
        (seconds / 86_400.0, "d")
    } else if absolute >= 3_600.0 {
        (seconds / 3_600.0, "h")
    } else if absolute >= 60.0 {
        (seconds / 60.0, "min")
    } else if absolute >= 1.0 {
        (seconds, "s")
    } else if absolute >= 0.001 {
        (seconds * 1_000.0, "ms")
    } else {
        (seconds * 1_000_000.0, "µs")
    };
    format!(
        "{scaled:.places$} {unit}",
        places = usize::from(places.min(8))
    )
}

fn format_size(value: f64) -> String {
    let size = crate::format::size(crate::num::u64_of_f64(value.abs().round()));
    if value <= -0.5 {
        format!("-{size}")
    } else {
        size
    }
}

fn format_bytes(value: f64, places: u8) -> String {
    crate::format::Formats::current().memory_value(value, usize::from(places.min(8)))
}

impl PlotMarks for CartesianMarks {
    type Hover = CartesianHover;
    type Activated = CartesianActivation;
    const NAME: &'static str = "CartesianPlot";
    const LABEL: &'static str = "Plot";

    fn frame(&self) -> &PlotFrame<CartesianHover> {
        &self.frame
    }
    fn frame_mut(&mut self) -> &mut PlotFrame<CartesianHover> {
        &mut self.frame
    }
    fn hit(&self, point: Point) -> Option<CartesianHover> {
        self.hit(point)
    }
    fn keyboard_items(&self) -> Vec<CartesianHover> {
        let (x_scale, y_scale) = (self.style.x_axis.scale, self.style.y_axis.scale);
        self.series
            .iter()
            .enumerate()
            .filter(|(_, series)| !self.toggles.is_hidden(&series.id))
            .flat_map(|(series, values)| {
                values
                    .data
                    .iter()
                    .enumerate()
                    .filter(|(_, datum)| {
                        Self::transform(datum.x, x_scale).is_some()
                            && Self::transform(datum.y, y_scale).is_some()
                    })
                    .map(move |(datum, _)| CartesianHover::Datum { series, datum })
            })
            .collect()
    }
    fn anchor(&self, hover: &CartesianHover) -> Option<Point> {
        let plot = self.plot_rect();
        let xd = self.domain(&self.style.x_axis, true);
        let yd = self.domain(&self.style.y_axis, false);
        match *hover {
            CartesianHover::Datum { series, datum } => {
                self.map_animated_datum(series, datum, plot, xd, yd, crate::time::now())
            }
            CartesianHover::Annotation(index) => self.annotation_anchor(index, plot, xd, yd),
        }
    }
    fn activation(&self, hover: &CartesianHover) -> Option<CartesianActivation> {
        match *hover {
            CartesianHover::Datum { series, datum } => {
                let values = self.series.get(series)?;
                let datum = values.data.get(datum)?;
                Some(CartesianActivation::Datum {
                    series: values.id.clone(),
                    datum: datum.id.clone(),
                })
            }
            CartesianHover::Annotation(index) => self
                .annotations
                .get(index)
                .filter(|annotation| annotation.interactive)
                .map(|annotation| CartesianActivation::Annotation(annotation.id.clone())),
        }
    }
    fn items(&self) -> Vec<PlotItem<CartesianActivation>> {
        let (x_scale, y_scale) = (self.style.x_axis.scale, self.style.y_axis.scale);
        let points = self
            .series
            .iter()
            .filter(|series| !self.toggles.is_hidden(&series.id))
            .flat_map(|series| {
                series
                    .data
                    .iter()
                    .filter(|datum| {
                        Self::transform(datum.x, x_scale).is_some()
                            && Self::transform(datum.y, y_scale).is_some()
                    })
                    .map(|datum| {
                        PlotItem::new(
                            format!("{}:{}", series.id, datum.id),
                            if datum.label.is_empty() {
                                format!("{} data point", series.name)
                            } else {
                                datum.label.clone()
                            },
                            if datum.detail.is_empty() {
                                format!("x {}, y {}", datum.x, datum.y)
                            } else {
                                datum.detail.clone()
                            },
                            CartesianActivation::Datum {
                                series: series.id.clone(),
                                datum: datum.id.clone(),
                            },
                        )
                    })
            });
        let annotations = self
            .annotations
            .iter()
            .filter(|annotation| annotation.interactive)
            .map(|annotation| {
                PlotItem::new(
                    format!("plot-annotation:{}", annotation.id),
                    if annotation.label.is_empty() {
                        annotation.id.clone()
                    } else {
                        annotation.label.clone()
                    },
                    if annotation.detail.is_empty() {
                        "Chart annotation".to_owned()
                    } else {
                        annotation.detail.clone()
                    },
                    CartesianActivation::Annotation(annotation.id.clone()),
                )
            });
        points.chain(annotations).collect()
    }
    fn series(&self) -> Vec<(&str, &str)> {
        self.series
            .iter()
            .map(|series| (series.id.as_str(), series.name.as_str()))
            .collect()
    }
    fn legend_interactive(&self) -> bool {
        self.style.interactive_legend && self.style.show_legend
    }
    fn toggles(&self) -> Option<&chart_legend::SeriesToggles> {
        Some(&self.toggles)
    }
    fn toggles_mut(&mut self) -> Option<&mut chart_legend::SeriesToggles> {
        Some(&mut self.toggles)
    }
    fn series_of(&self, hover: &CartesianHover) -> Option<&str> {
        match *hover {
            CartesianHover::Datum { series, .. } => self.series.get(series).map(|s| s.id.as_str()),
            CartesianHover::Annotation(_) => None,
        }
    }
    fn is_animating(&self) -> bool {
        !self.reduce_motion
            && self.style.animation.enabled
            && self.transition_from.is_some()
            && crate::time::now().saturating_duration_since(self.transition_started)
                < crate::widget::pie_chart::data_change_time()
    }
    fn wants_pointer_capture(&self) -> bool {
        self.pan_last.is_some() || self.brush_start.is_some()
    }
    fn visible_changed(&mut self) {
        self.invalidate_domains();
    }
    fn event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) -> bool {
        let claim = |ctx: &mut crate::widget::EventCtx<'_>| {
            *ctx.handled = true;
            *ctx.focus_target = Some(ctx.node_id);
        };
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                if self.drag_to(ctx.logical(position)) {
                    *ctx.handled = true;
                    return true;
                }
            }
            WindowEvent::Focused(false) => {
                self.pan_last = None;
                self.brush_start = None;
                self.frame.inspection.clear();
            }
            WindowEvent::MouseWheel { delta, .. }
                if self.style.zoom != CartesianZoom::Disabled
                    && (!self.style.gestures.zoom_modifier_required
                        || ctx.modifiers.state().control_key()
                        || ctx.modifiers.state().super_key()) =>
            {
                let amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    MouseScrollDelta::PixelDelta(position) => position.y / 80.0,
                };
                if self.plot_rect().contains(self.frame.inspection.pointer) && amount != 0.0 {
                    self.zoom_at_pointer(amount);
                    claim(ctx);
                }
            }
            WindowEvent::PinchGesture { delta, .. }
                if self.style.zoom != CartesianZoom::Disabled
                    && self.plot_rect().contains(self.frame.inspection.pointer) =>
            {
                // A positive delta means magnification. Pinch is already an
                // unambiguous zoom gesture, so it never requires a modifier.
                self.zoom_at_pointer(*delta);
                claim(ctx);
            }
            WindowEvent::PanGesture { delta, .. }
                if self.style.pan != CartesianPan::Disabled
                    && self.plot_rect().contains(self.frame.inspection.pointer)
                    && (!self.style.gestures.pan_modifier_required
                        || ctx.modifiers.state().alt_key()) =>
            {
                if self.pan_view_pixels(f64::from(delta.x), f64::from(delta.y), self.plot_rect()) {
                    self.frame.inspection.clear();
                    claim(ctx);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.begin_pointer_gesture(*ctx.modifiers) => claim(ctx),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.finish_pointer_gesture() => claim(ctx),
            _ => {}
        }
        *ctx.handled
    }
    /// Measure the bands around the plot in `theme`, then place the legend in
    /// what they leave: the layout runs this, so a hover on a mark or a press
    /// on the legend lands where the chart draws them before it has painted.
    fn place(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        self.bands = self.measure_bands(tr, theme);
        self.place_legend();
    }
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.reduce_motion = theme.reduce_motion;
        let now = crate::time::now();
        self.follow_keyboard_anchor();
        let plot = self.plot_rect();
        let xd = self.domain(&self.style.x_axis, true);
        let yd = self.domain(&self.style.y_axis, false);
        crate::widget::chart_navigation::draw_title(
            scene,
            transform,
            tr,
            theme,
            self.frame.rect,
            &self.frame.title,
        );
        self.draw_axes(scene, transform, tr, theme, plot, xd, yd);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &plot);
        self.draw_annotations(&mut AnnotationPaintContext {
            scene,
            transform,
            text: tr,
            theme,
            plot,
            x_domain: xd,
            y_domain: yd,
        });
        self.draw_series(scene, transform, theme, plot, xd, yd, now);
        if let Some(brush) = self.brush_rect() {
            scene.fill(
                Fill::NonZero,
                transform,
                with_alpha(theme.tokens.colors.intent.accent_ink, 42),
                None,
                &brush,
            );
            scene.stroke(
                &Stroke::new(1.5),
                transform,
                theme.tokens.colors.border_focus,
                None,
                &brush,
            );
        }
        scene.pop_layer();
        self.draw_legend(scene, transform, tr, theme);
    }
    fn readout(&self, theme: &Theme) -> Option<Readout> {
        self.readout_of(theme)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::PresenceCtx;
    use crate::widget::Widget;
    use crate::widget::chart_navigation::InteractiveChart as _;

    fn band_plot() -> CartesianPlot {
        let series = [
            "Storage over time",
            "Second long series",
            "Third long series",
        ]
        .iter()
        .enumerate()
        .map(|(index, name)| {
            PlotSeries::new(
                format!("series-{index}"),
                *name,
                Color::WHITE,
                vec![PlotDatum::new("a", 1.0, 2.0), PlotDatum::new("b", 9.0, 6.0)],
            )
        })
        .collect();
        CartesianPlot::new(series)
            .titled("Capacity")
            .with_height(300.0)
            .with_annotations([CartesianAnnotation::x_band(
                "middle",
                4.0,
                6.0,
                Color::WHITE,
            )])
    }

    fn draw_once(plot: &mut CartesianPlot, theme: &Theme) {
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        plot.draw(&mut crate::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            theme,
            PresenceCtx::default().draw_env(),
        ));
    }

    tilo_test_env::scenario! {
        /// Where a plot and its legend stand once laid out.
        /// Each phase was a test of its own and builds its own fixture.
        fn a_cartesian_plot_places_itself_and_its_legend;

        phase a_plot_draws_where_the_layout_placed_it;
        {
            let theme = Theme::dark();
            let (mut laid_out, mut painted) = (band_plot(), band_plot());
            for chart in [&mut laid_out, &mut painted] {
                crate::testing::lay_out_in(chart, 360.0, 300.0, &theme).expect("the plot lays out");
            }
            draw_once(&mut painted, &theme);
            assert_eq!(
                laid_out.marks.plot_rect(),
                painted.marks.plot_rect(),
                "painting adds no places"
            );

            let plot = painted.marks.plot_rect();
            let middle = Point::new(plot.center().x, plot.center().y);
            // Over the band drawn across the middle, the plot reads every series at the nearest
            // x position and names the band under them.
            let hovered = laid_out.marks.hit(middle);
            assert!(matches!(hovered, Some(CartesianHover::Datum { .. })), "{hovered:?}");
            assert_eq!(laid_out.marks.hit(middle), painted.marks.hit(middle));
            laid_out.marks.frame.inspection.pointer = middle;
            laid_out.marks.frame.inspection.hovered = hovered;
            let readout = laid_out.marks.readout_of(&theme).expect("a readout");
            assert_eq!(readout.rows.len(), 4, "three series and the band: {readout:?}");
            assert_eq!(readout.rows[3].label, "middle");

            let legend = laid_out.marks.frame.rect.y0 + laid_out.marks.bands.legend_top + 4.0;
            assert_eq!(
                laid_out.marks.hit(Point::new(middle.x, legend)),
                None,
                "the legend's rows hold no mark"
            );
        }

        phase a_legend_entry_takes_a_press_before_anything_paints;
        {
            let theme = Theme::dark();
            let mut chart = band_plot();
            crate::testing::lay_out_in(&mut chart, 360.0, 300.0, &theme).expect("the plot lays out");
            let ids: Vec<String> = chart.marks.series.iter().map(|s| s.id.clone()).collect();
            for id in ids {
                let entry = chart
                    .marks
                    .toggles
                    .entry(&id)
                    .unwrap_or_else(|| panic!("the layout placed {id}'s legend entry"));
                assert!(
                    chart.toggle_legend_at(entry.center()),
                    "a press on {id}'s entry toggles it"
                );
                assert!(!chart.is_series_visible(&id));
            }
        }

        phase a_series_change_places_the_legend_again;
        {
            let theme = Theme::dark();
            let mut chart = band_plot();
            crate::testing::lay_out_in(&mut chart, 360.0, 300.0, &theme).expect("the plot lays out");
            Widget::layout_measured(&mut chart);
            assert!(!Widget::is_layout_dirty(&chart));
            let before = chart.marks.toggles.entry("series-0").expect("placed");

            chart.set_series_immediate(
                [(
                    "series-0",
                    "Storage over time, every device and every backup",
                )]
                .into_iter()
                .map(|(id, name)| {
                    PlotSeries::new(id, name, Color::WHITE, vec![PlotDatum::new("a", 1.0, 2.0)])
                })
                .collect(),
            );
            assert!(
                Widget::is_layout_dirty(&chart),
                "new data re-places the legend"
            );
            crate::testing::lay_out_in(&mut chart, 360.0, 300.0, &theme).expect("the plot lays out");
            let after = chart.marks.toggles.entry("series-0").expect("placed again");
            assert!(
                after.width() >= before.width(),
                "the longer name takes at least the area the short one did: {before:?} then {after:?}"
            );
        }
    }

    tilo_test_env::scenario! {
        /// A plot's domains, ticks, titles, scales and series.
        /// Each phase was a test of its own and builds its own fixture.
        fn cartesian_plot_axes_and_series_keep_their_data;

        phase auto_domains_and_ticks_expand_preserve_and_thin;
        {
            fn narrow_axes_reduce_tick_density_but_keep_endpoints() {
                let axis = PlotAxis::default().with_ticks(
                    (0..10).map(|value| AxisTick::new(f64::from(value), value.to_string())),
                );
                let ticks = CartesianMarks::ticks(&axis, Domain::normalized(0.0, 9.0), 150.0);
                assert_eq!(ticks.len(), 2);
                assert_eq!(ticks.first().map(|tick| tick.label.as_str()), Some("0"));
                assert_eq!(ticks.last().map(|tick| tick.label.as_str()), Some("9"));
            }

            fn series_visibility_is_observable_and_changes_auto_domain() {
                let color = Color::from_rgb8(0, 0, 0);
                let mut plot = CartesianPlot::new(vec![
                    PlotSeries::new("near", "Near", color, vec![PlotDatum::new("a", 0.0, 1.0)]),
                    PlotSeries::new("far", "Far", color, vec![PlotDatum::new("b", 100.0, 100.0)]),
                ]);
                let before = plot.marks.base_domain(&PlotAxis::default(), true);
                assert!(plot.set_series_visible("far", false));
                let after = plot.marks.base_domain(&PlotAxis::default(), true);
                assert!(after.max < before.max);
                assert_eq!(plot.take_visibility_changed(), Some(("far".into(), false)));
                assert!(!plot.is_series_visible("far"));
            }

            narrow_axes_reduce_tick_density_but_keep_endpoints();
            series_visibility_is_observable_and_changes_auto_domain();
        }

        phase axis_titles_formats_and_tooltips_fit_the_chart;
        {
            fn axis_titles_wrap_to_be_read_whole_and_elide_only_past_two_lines() {
                let theme = Theme::light();
                let style = theme.text(TextRole::Caption).with_weight(TextWeight::Semibold);
                crate::text_render::with_layout_renderer(&theme, |tr| {
                    let lines = |tr: &mut TextRenderer, title: &str, available: f64| {
                        CartesianMarks::axis_title_lines(tr, &theme, title, available)
                    };
                    assert_eq!(lines(tr, "Latency", 200.0), vec!["Latency".to_owned()]);
                    let title = "Requests, errors and bandwidth";
                    let whole = f64::from(tr.measure_style(title, None, &style).0);
                    let wrapped = lines(tr, title, whole * 0.6);
                    assert_eq!(wrapped.len(), 2, "{wrapped:?}");
                    assert_eq!(wrapped.join(" "), title, "every word is read");
                    for line in &wrapped {
                        assert!(
                            f64::from(tr.measure_style(line, None, &style).0) <= whole * 0.6,
                            "{line} fits the plot's height"
                        );
                    }
                    let long = "Requests per second, errors per second and bandwidth used";
                    let elided = lines(tr, long, whole * 0.4);
                    assert_eq!(elided.len(), 2, "{elided:?}");
                    assert!(elided[1].ends_with('…'), "{elided:?}");
                    assert!(lines(tr, "", 200.0).is_empty());
                });
            }

            axis_titles_wrap_to_be_read_whole_and_elide_only_past_two_lines();
        }

    }

    tilo_test_env::scenario! {
        /// Zoom and pan, hover and the brush of a plot.
        /// Each phase was a test of its own and builds its own fixture.
        fn a_cartesian_plot_navigates_hovers_and_brushes;

        phase hover_annotations_and_live_updates_are_exact_and_stable;
        {
            fn sorted_series_hover_uses_a_small_exact_x_window() {
                let series = PlotSeries::new(
                    "sorted",
                    "Sorted",
                    Color::WHITE,
                    (0..10_000)
                        .map(|index| PlotDatum::new(index.to_string(), f64::from(index), 1.0))
                        .collect(),
                )
                .sorted_by_x();
                let mut plot = CartesianPlot::new(vec![series.clone()]);
                plot.marks.frame.inspection.pointer = Point::new(500.0, 50.0);
                plot.marks.style.hit_radius = 10.0;
                let range = plot.marks.hover_range(
                    &series,
                    Rect::new(0.0, 0.0, 1_000.0, 100.0),
                    Domain::normalized(0.0, 10_000.0),
                );
                assert!(range.contains(&5_000));
                assert!(range.len() < 250, "{} candidates", range.len());
            }

            fn annotations_are_hit_testable_and_programmatically_inspectable() {
                let color = Color::WHITE;
                let mut plot = CartesianPlot::new(Vec::new()).with_annotations([
                    CartesianAnnotation::x_line("marker", 5.0, color).with_detail("Release"),
                    CartesianAnnotation::x_band("window", 7.0, 9.0, color),
                    CartesianAnnotation::y_line("decoration", 2.0, color).interactive(false),
                ]);
                plot.marks.style.x_axis = PlotAxis::default().fixed(0.0, 10.0);
                plot.marks.style.y_axis = PlotAxis::default().fixed(0.0, 10.0);
                let area = Rect::new(0.0, 0.0, 100.0, 100.0);
                let domain = Domain::normalized(0.0, 10.0);
                assert_eq!(
                    plot.marks
                        .annotation_at(Point::new(50.0, 40.0), area, domain, domain),
                    Some(0)
                );
                assert_eq!(
                    plot.marks
                        .annotation_at(Point::new(80.0, 40.0), area, domain, domain),
                    Some(1)
                );
                assert!(plot.inspect_annotation("marker"));
                assert!(!plot.inspect_annotation("decoration"));
                assert!(!plot.inspect_annotation("missing"));
            }

            fn stable_ids_interpolate_live_updates_and_reduced_motion_snaps() {
                let color = Color::from_rgb8(1, 2, 3);
                let old = PlotSeries::new(
                    "series",
                    "Series",
                    color,
                    vec![PlotDatum::new("point", 0.0, 10.0)],
                );
                let mut plot = CartesianPlot::new(vec![old]);
                plot.set_series(vec![PlotSeries::new(
                    "series",
                    "Series",
                    color,
                    vec![PlotDatum::new("point", 100.0, 30.0)],
                )]);
                let midpoint = crate::time::now();
                plot.marks.transition_started = midpoint
                    .checked_sub(crate::time::Duration::from_millis(100))
                    .unwrap();
                let (x, y) = plot.marks.animated_xy(0, 0, midpoint).unwrap();
                assert!(x > 50.0 && x < 100.0, "ease-out midpoint: {x}");
                assert!(y > 20.0 && y < 30.0, "ease-out midpoint: {y}");
                assert!(plot.needs_animation_frame());
                plot.marks.reduce_motion = true;
                assert_eq!(plot.marks.animated_xy(0, 0, midpoint), Some((100.0, 30.0)));
                assert!(!plot.needs_animation_frame());
            }

            sorted_series_hover_uses_a_small_exact_x_window();
            annotations_are_hit_testable_and_programmatically_inspectable();
            stable_ids_interpolate_live_updates_and_reduced_motion_snaps();
        }

    }

    tilo_test_env::scenario! {
        /// A histogram's bins stay apart as its plot narrows to a phone's width.
        fn histogram_bins_keep_a_gap_at_a_phone_width;

        phase bins_never_touch_at_a_phone_width_whatever_the_device_scale;
        {
            let theme = Theme::dark();
            // Twelve bins of ten readings each. Drawn at the width a desktop plot gives
            // them, the bars would overlap this narrow a plot and read as one silhouette.
            let samples = (0..120)
                .map(|index| 80.0 + f64::from(index % 12) * 3.0)
                .collect::<Vec<_>>();
            for scale in [1.0_f64, 1.5] {
                let _held = crate::widget::enter_device_scale(scale);
                let mut chart = crate::widget::histogram::Histogram::new(
                    samples.iter().copied(),
                    Color::WHITE,
                )
                .bins(12)
                .build();
                crate::testing::lay_out_in(&mut chart, 342.0, 300.0, &theme)
                    .expect("the histogram lays out");
                let bars = chart.marks.bars_of("histogram");
                assert_eq!(
                    bars.len(),
                    12,
                    "every bin draws a bar at scale {scale}: {bars:?}"
                );
                let device = 1.0 / scale;
                for pair in bars.windows(2) {
                    let gap = pair[1].x0 - pair[0].x1;
                    assert!(
                        gap >= device - 1e-9,
                        "bins stay a device pixel apart at scale {scale}: {gap} between {pair:?}"
                    );
                }
            }
        }
    }
}
